//! Thin CLI that wraps the cmux-compatible JSON-RPC protocol. Mirrors
//! the subset of cmux's official CLI commands that we use for testing,
//! scripting, and the eventual claude-code teammateMode handshake.
//!
//! Subcommands map 1:1 to protocol methods:
//!
//!   kasaterm-cli ping
//!   kasaterm-cli capabilities
//!   kasaterm-cli identify
//!   kasaterm-cli list workspaces|surfaces
//!   kasaterm-cli focus  <surface_id>
//!   kasaterm-cli split  <left|right|up|down>
//!   kasaterm-cli send   <text>                  # writes to focused pane
//!   kasaterm-cli send   <surface_id> <text>     # writes to specific pane
//!   kasaterm-cli key    <enter|tab|...>
//!
//! Socket path resolution mirrors what the host exports:
//!   $KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > platform default
//! Platform default is `/tmp/cmux.sock` on Unix and `\\.\pipe\cmux` on
//! Windows — same name choice the host uses when no env override is
//! present.
//!
//! The CLI prints the raw JSON response to stdout — scripts pipe it
//! through `jq`. Exit code is 0 on `ok: true`, 1 on `ok: false`, 2 on
//! a transport / framing error.

use agent_socket::protocol::{Request, Response};
use agent_socket::transport::LocalStream;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

fn main() {
    match run() {
        Ok(Some(resp)) => {
            // Print the wire response so scripts can `| jq .result`.
            println!("{}", serde_json::to_string(&resp).unwrap());
            std::process::exit(if resp.ok { 0 } else { 1 });
        }
        Ok(None) => {} // help / version path — already printed.
        Err(e) => {
            eprintln!("kasaterm-cli: {e:#}");
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
    eprintln!("  kasaterm-cli ping");
    eprintln!("  kasaterm-cli capabilities");
    eprintln!("  kasaterm-cli identify");
    eprintln!("  kasaterm-cli list <workspaces|surfaces>");
    eprintln!("  kasaterm-cli focus <surface_id>");
    eprintln!("  kasaterm-cli close <surface_id>");
    eprintln!("  kasaterm-cli rename <surface_id> <title>");
    eprintln!("  kasaterm-cli color <surface_id> <#rrggbb>");
    eprintln!("  kasaterm-cli split <left|right|up|down>");
    eprintln!("  kasaterm-cli swap  <surface_a> <surface_b>");
    eprintln!("  kasaterm-cli send  <text>");
    eprintln!("  kasaterm-cli send  --surface <id> <text>");
    eprintln!("  kasaterm-cli key   <enter|tab|escape|backspace|delete|up|down|left|right>");
    eprintln!("  kasaterm-cli announce <intent> [status]   # publish what THIS pane is doing");
    eprintln!("  kasaterm-cli board                        # what every pane is doing");
    eprintln!("  kasaterm-cli peek  [surface_id] [lines]   # read a pane's visible screen");
    eprintln!("  kasaterm-cli bind-transcript <path>       # register THIS pane's claude transcript (hook)");
    eprintln!();
    eprintln!(
        "Socket: $KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > platform default (Unix /tmp/cmux.sock, Windows \\\\.\\pipe\\cmux)"
    );
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
        "close" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("close needs a surface_id"))?;
            ("surface.close", json!({ "surface_id": surface }))
        }
        "rename" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("rename needs <surface_id> <title>"))?;
            let title = args
                .get(1)
                .ok_or_else(|| anyhow!("rename needs a title"))?;
            (
                "surface.rename",
                json!({ "surface_id": surface, "title": title }),
            )
        }
        "color" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("color needs <surface_id> <#rrggbb>"))?;
            let color = args
                .get(1)
                .ok_or_else(|| anyhow!("color needs a #rrggbb value"))?;
            (
                "surface.set_color",
                json!({ "surface_id": surface, "color": color }),
            )
        }
        "split" => {
            let dir = args
                .first()
                .ok_or_else(|| anyhow!("split needs a direction"))?;
            ("surface.split", json!({ "direction": dir }))
        }
        "swap" => {
            let a = args
                .first()
                .ok_or_else(|| anyhow!("swap needs <surface_a> <surface_b>"))?;
            let b = args
                .get(1)
                .ok_or_else(|| anyhow!("swap needs a second surface_id"))?;
            ("surface.swap", json!({ "a": a, "b": b }))
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
        "announce" => {
            // The pane announces *itself*: surface_id comes from the env
            // the host injects, so a script inside a pane never has to
            // figure out its own id.
            let surface = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                anyhow!("announce needs $KASATERM_PANE_ID (run inside a kasaterm pane)")
            })?;
            let intent = args
                .first()
                .ok_or_else(|| anyhow!("announce needs an <intent> string"))?
                .clone();
            let status = args.get(1).cloned().unwrap_or_else(|| "working".to_string());
            (
                "collab.announce",
                json!({ "surface_id": surface, "intent": intent, "status": status }),
            )
        }
        "board" => ("collab.board", json!({})),
        "bind-transcript" => {
            // The pane registers its own transcript: surface_id from the
            // host-injected env, path from the hook's stdin (passed as the
            // arg). Lets the host tail it and auto-fill the board.
            let surface = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                anyhow!("bind-transcript needs $KASATERM_PANE_ID (run inside a kasaterm pane)")
            })?;
            let path = args
                .first()
                .ok_or_else(|| anyhow!("bind-transcript needs a <transcript_path>"))?
                .clone();
            (
                "collab.bind_transcript",
                json!({ "surface_id": surface, "path": path }),
            )
        }
        "peek" => {
            // Default to this pane if no id given — handy for "what does my
            // own screen look like" but the usual case is peeking a sibling.
            let surface = args
                .first()
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok())
                .ok_or_else(|| anyhow!("peek needs a surface_id (or $KASATERM_PANE_ID)"))?;
            let mut params = json!({ "surface_id": surface });
            if let Some(lines) = args.get(1).and_then(|s| s.parse::<u64>().ok()) {
                params["lines"] = json!(lines);
            }
            ("surface.peek", params)
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
    // Per-platform default avoids carrying a Unix-only `/tmp/...` path
    // into Windows builds, where pipe names live in their own
    // namespace.
    #[cfg(unix)]
    let default = "/tmp/cmux.sock".to_string();
    #[cfg(windows)]
    let default = r"\\.\pipe\cmux".to_string();
    Ok(std::env::var("KASATERM_SOCKET_PATH")
        .or_else(|_| std::env::var("CMUX_SOCKET_PATH"))
        .unwrap_or(default))
}

fn roundtrip(socket_path: &str, request: &Request) -> Result<Response> {
    let stream = LocalStream::connect(Path::new(socket_path))
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
