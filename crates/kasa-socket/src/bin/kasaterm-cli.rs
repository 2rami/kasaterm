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

use kasa_socket::protocol::{Request, Response};
use kasa_socket::transport::LocalStream;
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
    // `board-watch` is a polling loop, not a single round-trip: it streams one
    // line per *changed* pane so a Claude Code Monitor can watch the board and
    // wake on transitions (a worker going `waiting` for a permission prompt,
    // finishing → `idle`, etc.) without dumping the whole board every tick.
    if cmd == "board-watch" {
        let interval = args.first().and_then(|s| s.parse::<u64>().ok()).unwrap_or(3);
        let socket_path = resolve_socket_path()?;
        run_board_watch(&socket_path, interval)?;
        return Ok(None);
    }
    let request = build_request(&cmd, &args)?;
    let socket_path = resolve_socket_path()?;
    let response = roundtrip(&socket_path, &request)?;
    // `layout` is meant to be *read*, not piped — render the pane rects as an
    // ASCII diagram so claude (and a human) grasp the screen split at a glance.
    // On error we fall through to the raw JSON so the failure is still visible.
    if cmd == "layout" && response.ok {
        println!("{}", render_layout(&response));
        return Ok(None);
    }
    // `windows` lists every window (not just the visible one) so an agent can
    // answer "what's in window 1" — each gets a header + its own box diagram.
    if cmd == "windows" && response.ok {
        println!("{}", render_windows(&response));
        return Ok(None);
    }
    Ok(Some(response))
}

/// Poll `collab.board` every `interval_secs` and print one line per pane whose
/// status/intent changed since the last tick (plus `closed` for panes that
/// vanished). Each line is a Monitor event. The first tick emits the current
/// board as the baseline. Transient socket failures are swallowed — one dropped
/// poll must not kill a long-running watch. Never returns (Ctrl-C / Monitor
/// timeout ends it).
fn run_board_watch(socket_path: &str, interval_secs: u64) -> Result<()> {
    use std::collections::BTreeMap;
    let req = Request {
        id: "board-watch".into(),
        method: "collab.board".into(),
        params: json!({}),
    };
    let mut prev: BTreeMap<String, String> = BTreeMap::new();
    loop {
        if let Ok(resp) = roundtrip(socket_path, &req) {
            if let Some(board) = resp
                .result
                .as_ref()
                .and_then(|v| v.get("board"))
                .and_then(|v| v.as_array())
            {
                let mut cur: BTreeMap<String, String> = BTreeMap::new();
                for e in board {
                    let id = e
                        .get("surface_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let intent = e.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                    let waiting = e.get("waiting_for").and_then(|v| v.as_str());
                    let line = match waiting {
                        Some(w) => format!("{status} (waiting: {w}) — {intent}"),
                        None => format!("{status} — {intent}"),
                    };
                    cur.insert(id, line);
                }
                let mut out = std::io::stdout().lock();
                for (id, line) in &cur {
                    if prev.get(id) != Some(line) {
                        let _ = writeln!(out, "{id}  {line}");
                    }
                }
                for id in prev.keys() {
                    if !cur.contains_key(id) {
                        let _ = writeln!(out, "{id}  closed");
                    }
                }
                let _ = out.flush();
                prev = cur;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(interval_secs.max(1)));
    }
}

/// Render `window.list`'s windows as a labelled stack of box diagrams, one
/// per window, with the active one marked.
fn render_windows(resp: &Response) -> String {
    let windows = resp
        .result
        .as_ref()
        .and_then(|v| v.get("windows"))
        .and_then(|v| v.as_array());
    let Some(arr) = windows else {
        return "(윈도우 정보 없음)".to_string();
    };
    if arr.is_empty() {
        return "(윈도우 없음)".to_string();
    }
    let mut out = String::new();
    for w in arr {
        let idx = w.get("idx").and_then(|v| v.as_u64()).unwrap_or(0);
        let active = w.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
        let surfaces: Vec<String> = w
            .get("surfaces")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let mark = if active { "  ← 현재 보이는 윈도우" } else { "" };
        out.push_str(&format!(
            "■ 윈도우 {idx}{mark}\n  pane: {}\n",
            surfaces.join(" ")
        ));
        let rects: Vec<(String, u16, u16, u16, u16)> = w
            .get("panes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        Some((
                            p.get("surface_id")?.as_str()?.to_string(),
                            p.get("x")?.as_u64()? as u16,
                            p.get("y")?.as_u64()? as u16,
                            p.get("w")?.as_u64()? as u16,
                            p.get("h")?.as_u64()? as u16,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !rects.is_empty() {
            for line in draw_boxes(&rects).lines() {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Render `window.layout`'s pane rects (window-relative %) as a box diagram.
fn render_layout(resp: &Response) -> String {
    let panes = resp
        .result
        .as_ref()
        .and_then(|v| v.get("panes"))
        .and_then(|v| v.as_array());
    let rects: Vec<(String, u16, u16, u16, u16)> = match panes {
        Some(arr) => arr
            .iter()
            .filter_map(|p| {
                Some((
                    p.get("surface_id")?.as_str()?.to_string(),
                    p.get("x")?.as_u64()? as u16,
                    p.get("y")?.as_u64()? as u16,
                    p.get("w")?.as_u64()? as u16,
                    p.get("h")?.as_u64()? as u16,
                ))
            })
            .collect(),
        None => Vec::new(),
    };
    if rects.is_empty() {
        return "(빈 레이아웃 — 보이는 pane 없음)".to_string();
    }
    draw_boxes(&rects)
}

/// Box-drawing from pane rects given as 0..100 percentages of the window.
/// Each cell accumulates U/D/L/R connection bits so shared borders between
/// adjacent panes resolve to the right junction glyph (┬ ├ ┼ …) automatically.
fn draw_boxes(rects: &[(String, u16, u16, u16, u16)]) -> String {
    const W: usize = 46;
    const H: usize = 15;
    const U: u8 = 1;
    const D: u8 = 2;
    const L: u8 = 4;
    const R: u8 = 8;
    // % → cell (round). Width/height map onto the last index so 100% lands
    // on the far edge.
    let cx = |p: u16| -> usize { (p as usize * (W - 1) + 50) / 100 };
    let cy = |p: u16| -> usize { (p as usize * (H - 1) + 50) / 100 };

    let mut bits = vec![vec![0u8; W]; H];
    let mut labels: Vec<(usize, usize, String)> = Vec::new();
    for (id, x, y, w, h) in rects {
        let x0 = cx(*x);
        let y0 = cy(*y);
        let x1 = cx(x + w).min(W - 1).max(x0 + 2);
        let y1 = cy(y + h).min(H - 1).max(y0 + 2);
        for xx in (x0 + 1)..x1 {
            bits[y0][xx] |= L | R;
            bits[y1][xx] |= L | R;
        }
        for yy in (y0 + 1)..y1 {
            bits[yy][x0] |= U | D;
            bits[yy][x1] |= U | D;
        }
        bits[y0][x0] |= R | D;
        bits[y0][x1] |= L | D;
        bits[y1][x0] |= R | U;
        bits[y1][x1] |= L | U;
        labels.push(((y0 + y1) / 2, (x0 + x1) / 2, id.clone()));
    }
    let glyph = |b: u8| -> char {
        match b {
            0 => ' ',
            b if b == L | R => '─',
            b if b == U | D => '│',
            b if b == R | D => '┌',
            b if b == L | D => '┐',
            b if b == R | U => '└',
            b if b == L | U => '┘',
            b if b == L | R | D => '┬',
            b if b == L | R | U => '┴',
            b if b == U | D | R => '├',
            b if b == U | D | L => '┤',
            b if b == U | D | L | R => '┼',
            _ => '·',
        }
    };
    let mut grid: Vec<Vec<char>> = bits
        .iter()
        .map(|row| row.iter().map(|&b| glyph(b)).collect())
        .collect();
    for (cy_, cx_, id) in labels {
        let lab: Vec<char> = id.chars().collect();
        let sx = cx_.saturating_sub(lab.len() / 2);
        for (i, c) in lab.iter().enumerate() {
            let col = sx + i;
            if col < W && grid[cy_][col] == ' ' {
                grid[cy_][col] = *c;
            }
        }
    }
    grid.iter()
        .map(|r| r.iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
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
    eprintln!("  kasaterm-cli key   [--surface <id>] <enter|tab|escape|up|down|left|right|...>  # 특정 pane에 키/선택");
    eprintln!("  kasaterm-cli tell  <surface_id> <text>     # send + submit (wake an idle claude)");
    eprintln!("  kasaterm-cli board [screen_lines]         # what every pane is doing (+ screen tail if N given)");
    eprintln!("  kasaterm-cli board-watch [interval_s]     # stream changed pane status (1 line/change) — feed a Claude Code Monitor");
    eprintln!("  kasaterm-cli layout                       # where each pane sits (active window, %)");
    eprintln!("  kasaterm-cli windows                      # every window (sidebar order) + its panes");
    eprintln!("  kasaterm-cli peek  [surface_id] [lines]   # read a pane's visible screen");
    eprintln!("  kasaterm-cli transcript [surface_id] [N]  # last N turns (prompts+replies) of a pane's claude");
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
            // key [--surface <id>] <key-name> — send a key (enter/escape/up/
            // down/left/right/tab/...) to a specific pane, e.g. to answer an
            // AskUserQuestion from outside (arrow keys + enter). Without
            // --surface it targets the focused pane.
            let (surface, key) = if args.first().is_some_and(|a| a == "--surface") {
                (
                    Some(
                        args.get(1)
                            .ok_or_else(|| anyhow!("--surface needs an id"))?
                            .clone(),
                    ),
                    args.get(2)
                        .ok_or_else(|| anyhow!("key needs a key name"))?
                        .clone(),
                )
            } else {
                (
                    None,
                    args.first()
                        .ok_or_else(|| anyhow!("key needs a key name"))?
                        .clone(),
                )
            };
            let mut params = json!({ "key": key });
            if let Some(s) = surface {
                params["surface_id"] = json!(s);
            }
            ("surface.send_key", params)
        }
        "tell" => {
            // send + submit in one shot, so an idle claude in the target pane
            // wakes and acts on the message:  tell <surface_id> <text>
            let surface = args
                .first()
                .filter(|a| a.starts_with('%'))
                .cloned()
                .ok_or_else(|| anyhow!("tell needs <surface_id> <text> (e.g. tell %3 \"hi\")"))?;
            let text = args
                .get(1..)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("tell needs a text payload"))?
                .join(" ");
            // Flatten internal newlines so a stray \n can't fire a half-typed
            // turn, then append \r — claude submits on CR (0x0d); a bare \n
            // (0x0a) is only a newline insert, not a submit.
            let flat: String = text
                .chars()
                .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                .collect();
            // Prepend Ctrl+U (0x15): clear any half-typed line already resident
            // in the target's prompt before our message lands. Without it the
            // resident text and the tell body merge into one forced submit —
            // the user's in-progress input is lost and the message is polluted.
            // Ctrl+U only wipes the input buffer, so it stays safe mid-generation
            // (esc would interrupt the target claude instead).
            (
                "surface.send_text",
                json!({ "surface_id": surface, "text": format!("\x15{}\r", flat.trim()) }),
            )
        }
        "board" => {
            // Bare `board` = metadata only. `board <N>` folds each pane's
            // visible last N rows in — what an orchestrator pane reads to see
            // who's stuck on a prompt without a peek-per-pane.
            let mut params = json!({});
            if let Some(lines) = args.first().and_then(|s| s.parse::<u64>().ok()) {
                params["screen_lines"] = json!(lines);
            }
            ("collab.board", params)
        }
        "layout" => ("window.layout", json!({})),
        "windows" => ("window.list", json!({})),
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
        "transcript" => {
            // Structured dialogue of a sibling pane's claude: the last N turns
            // (user prompts + assistant replies), including ones scrolled off
            // the screen that `peek` can't reach.  transcript <surface_id> [N]
            let surface = args
                .first()
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok())
                .ok_or_else(|| anyhow!("transcript needs a surface_id (or $KASATERM_PANE_ID)"))?;
            let mut params = json!({ "surface_id": surface });
            if let Some(turns) = args.get(1).and_then(|s| s.parse::<u64>().ok()) {
                params["turns"] = json!(turns);
            }
            ("collab.transcript", params)
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
