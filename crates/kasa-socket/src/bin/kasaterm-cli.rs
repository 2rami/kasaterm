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
//!   kasaterm-cli split  <left|right|up|down> [--focus]
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
    // `wake-watch <surface>` blocks until ONE teammate finishes a turn, then
    // exits — the inverse of board-watch (which streams forever). Meant to run
    // as a Claude Code background task: its exit auto-re-invokes the idle pane
    // that launched it, so a worker waiting on a teammate wakes the instant the
    // teammate is done, without the input-line pollution `tell` causes.
    if cmd == "wake-watch" {
        let target = args
            .iter()
            .find(|a| a.starts_with('%'))
            .cloned()
            .ok_or_else(|| anyhow!("wake-watch needs <surface_id> (e.g. wake-watch %3)"))?;
        let interval = args
            .iter()
            .skip(1)
            .find_map(|s| s.parse::<u64>().ok())
            .unwrap_or(3);
        let timeout = args
            .iter()
            .position(|a| a == "--timeout")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1800);
        let socket_path = resolve_socket_path()?;
        run_wake_watch(&socket_path, &target, interval, timeout)?;
        return Ok(None);
    }
    // `sessions`/`resume` — 터미널 안 세션 피커. claude 자체 /resume 은 teamName 이
    // 기록된 세션(=팀 트리플로 뜨는 kasaterm pane 세션 전부)을 무조건 숨기므로,
    // jsonl 직스캔으로 팀 세션까지 전부 보여주고 학생색·학생명으로 구분한다.
    // 디스크만 읽어 GUI 가 죽어 있어도 동작. `resume` 은 번호를 받아 그 자리에서
    // `claude --resume` 을 실행한다(pane 이면 shim 이 트리플·페르소나 재부착).
    if cmd == "sessions" || cmd == "resume" {
        run_sessions_picker(cmd == "resume", &args)?;
        return Ok(None);
    }
    // `rename [sid] <이름>` — 세션 제목 변경. claude 자체 /rename 은 teammate(팀
    // 트리플로 뜬 pane 세션 전부)에서 차단되지만, 실체는 transcript jsonl 에
    // custom-title 레코드 한 줄 append 라 디스크에 직접 쓰면 같은 효과다.
    // 피커 라벨(parse_session_label)이 custom-title 을 최우선으로 읽는다.
    // 첫 인자가 %surface 면 기존 pane 개명(소켓 rename <surface_id> <title>)
    // 경로로 넘긴다 — 같은 이름의 선주민 명령을 가로채면 안 된다.
    if cmd == "rename" && args.first().is_none_or(|a| !a.starts_with('%')) {
        run_session_rename(&args)?;
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

/// Poll `collab.board` AND this pane's inbox every `interval_secs`, printing
/// one Monitor event line per change: a pane whose status/intent changed (or
/// `closed`), and a new `✉` message addressed to `$KASATERM_PANE_ID`. The first
/// tick baselines both (board state + existing messages) silently so a fresh
/// watch doesn't replay history. Transient failures are swallowed. Never
/// returns (Ctrl-C / Monitor timeout ends it).
fn run_board_watch(socket_path: &str, interval_secs: u64) -> Result<()> {
    use std::collections::{BTreeMap, HashSet};
    let req = Request {
        id: "board-watch".into(),
        method: "collab.board".into(),
        params: json!({}),
    };
    let me = std::env::var("KASATERM_PANE_ID").unwrap_or_default();
    let msgs_path = collab_messages_path();
    let mut prev: BTreeMap<String, String> = BTreeMap::new();
    let mut seen_msgs: HashSet<String> = HashSet::new();
    let mut first = true;
    loop {
        // --- board: status/intent changes + closed panes ---
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
                    if id == me {
                        continue; // 내 변화는 내가 이미 안다 — 노이즈 제거
                    }
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
        // --- inbox: new messages addressed to me (kasacollab msg) ---
        if std::env::var_os("KASATERM_BW_DEBUG").is_some() {
            eprintln!("[bw-dbg] me={me:?} path={msgs_path:?} exists={} seen={}",
                msgs_path.exists(), seen_msgs.len());
        }
        if !me.is_empty() {
            if let Ok(content) = std::fs::read_to_string(&msgs_path) {
                let mut out = std::io::stdout().lock();
                for line in content.lines() {
                    let Ok(m) = serde_json::from_str::<Value>(line) else { continue };
                    if m.get("to").and_then(|v| v.as_str()) != Some(me.as_str()) {
                        continue;
                    }
                    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if id.is_empty() || !seen_msgs.insert(id.to_string()) {
                        continue; // already seen (or baselined on first tick)
                    }
                    if !first {
                        let from = m.get("from").and_then(|v| v.as_str()).unwrap_or("?");
                        let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        let _ = writeln!(out, "✉ {from} → 나: {text}");
                    }
                }
                let _ = out.flush();
            }
        }
        first = false;
        std::thread::sleep(std::time::Duration::from_secs(interval_secs.max(1)));
    }
}

/// Poll `collab.board` until `target` finishes one turn, then print a single
/// line and RETURN (board-watch streams forever; this exits). Run as a Claude
/// Code background task so its exit auto-wakes the launching pane.
///
/// "Finished" = we saw `target` go working/busy/waiting at least once (armed),
/// then settle back to idle/success — or vanish from the board. If it never
/// starts within `timeout_secs`, exit with a timeout line so the waiter still
/// wakes and can decide. Transient socket failures are swallowed (keep polling).
fn run_wake_watch(
    socket_path: &str,
    target: &str,
    interval_secs: u64,
    timeout_secs: u64,
) -> Result<()> {
    let req = Request {
        id: "wake-watch".into(),
        method: "collab.board".into(),
        params: json!({}),
    };
    let interval = interval_secs.max(1);
    let max_ticks = (timeout_secs / interval).max(1);
    let mut armed = false;
    let mut ticks = 0u64;
    loop {
        let mut seen = false;
        if let Ok(resp) = roundtrip(socket_path, &req) {
            if let Some(board) = resp
                .result
                .as_ref()
                .and_then(|v| v.get("board"))
                .and_then(|v| v.as_array())
            {
                for e in board {
                    if e.get("surface_id").and_then(|v| v.as_str()) != Some(target) {
                        continue;
                    }
                    seen = true;
                    let status = e.get("status").and_then(|v| v.as_str()).unwrap_or("");
                    let intent = e.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                    let character = e.get("character").and_then(|v| v.as_str()).unwrap_or("");
                    let who = if character.is_empty() {
                        target.to_string()
                    } else {
                        format!("{character}({target})")
                    };
                    match status {
                        "working" | "busy" | "waiting" => armed = true,
                        "idle" | "success" if armed => {
                            let tail = if intent.is_empty() {
                                String::new()
                            } else {
                                format!(" — {intent}")
                            };
                            let mut out = std::io::stdout().lock();
                            let _ = writeln!(out, "{who} 작업 끝남{tail}");
                            let _ = out.flush();
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
        }
        // Vanished after we saw it run → closed/done. Wake the waiter.
        if armed && !seen {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{target} 사라짐 (pane closed) — 끝난 걸로 본다");
            let _ = out.flush();
            return Ok(());
        }
        ticks += 1;
        if ticks >= max_ticks {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(
                out,
                "{target} wake-watch {timeout_secs}s 타임아웃 — 아직 시작/완료 안 됨, 직접 확인 필요"
            );
            let _ = out.flush();
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
}

/// `/tmp/kasaterm-collab/<cwd-with-/-and-.-as-->/messages.jsonl` — the same
/// path kasacollab.py derives, so `board-watch` reads the inbox kasacollab msg
/// writes. cwd-dependent, so the watch must run from the project directory.
fn collab_messages_path() -> std::path::PathBuf {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let enc: String = cwd
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    std::path::Path::new("/tmp/kasaterm-collab")
        .join(enc)
        .join("messages.jsonl")
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
    eprintln!("cmux-compatible JSON-RPC CLI for kasaterm / agent-socket\n");
    eprintln!("Usage:");
    eprintln!("  kasaterm-cli ping");
    eprintln!("  kasaterm-cli capabilities");
    eprintln!("  kasaterm-cli identify");
    eprintln!("  kasaterm-cli list <workspaces|surfaces>");
    eprintln!("  kasaterm-cli focus <surface_id>");
    eprintln!("  kasaterm-cli close <surface_id>");
    eprintln!("  kasaterm-cli rename <surface_id> <title>");
    eprintln!("  kasaterm-cli rename-window <title>          # 이 pane 의 세션 이름");
    eprintln!("  kasaterm-cli color <surface_id> <#rrggbb>");
    eprintln!("  kasaterm-cli split <left|right|up|down> [--focus]  # 기본 no-focus");
    eprintln!("  kasaterm-cli swap  <surface_a> <surface_b>");
    eprintln!("  kasaterm-cli resize <surface_id> <ratio>   # 직계 split 에서 차지 비중 0..1 (오케스트레이터 크게)");
    eprintln!("  kasaterm-cli send  <text>");
    eprintln!("  kasaterm-cli send  --surface <id> <text>");
    eprintln!("  kasaterm-cli key   [--surface <id>] <enter|tab|escape|up|down|left|right|...>  # 특정 pane에 키/선택");
    eprintln!("  kasaterm-cli tell  <surface_id> <text>     # send + submit (wake an idle claude)");
    eprintln!("  kasaterm-cli board [screen_lines]         # what every pane is doing (+ screen tail if N given)");
    eprintln!("  kasaterm-cli board-watch [interval_s]     # stream changed pane status (1 line/change) — feed a Claude Code Monitor");
    eprintln!("  kasaterm-cli wake-watch <surface_id> [interval_s] [--timeout s]  # block until a teammate finishes one turn, then exit (run as a background task → auto-wakes you)");
    eprintln!("  kasaterm-cli layout                       # where each pane sits (active window, %)");
    eprintln!("  kasaterm-cli windows                      # every window (sidebar order) + its panes");
    eprintln!("  kasaterm-cli peek  [surface_id] [lines]   # read a pane's visible screen");
    eprintln!("  kasaterm-cli transcript [surface_id] [N]  # last N turns (prompts+replies) of a pane's claude");
    eprintln!("  kasaterm-cli bind-transcript <path>       # register THIS pane's claude transcript (hook)");
    eprintln!("  kasaterm-cli notify [--surface <id>] <title> [body]  # fire a work-complete notification (Stop hook)");
    eprintln!("  kasaterm-cli attention [--surface <id>] [reason]     # flag a pane blocked on a permission/input prompt (Notification hook)");
    eprintln!("  kasaterm-cli sessions [N]                 # 최근 claude 세션 목록(학생색·학생명, /resume 이 숨기는 팀 세션 포함)");
    eprintln!("  kasaterm-cli resume [N]                   # 위 목록에서 번호로 골라 그 자리에서 claude --resume");
    eprintln!("  kasaterm-cli rename [sid|sid8] <이름>     # 세션 제목 변경(teammate 세션 /rename 차단 우회, sid 생략=이 pane)");
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
        "rename-window" => {
            // 윈도우/세션 이름 변경. surface.rename 과 달리 surface_id 를 받지 않고
            // 호출한 pane($KASATERM_PANE_ID)이 속한 윈도우를 대상으로 한다 —
            // 오케스트레이터 pane 이 윈도우 라벨을 덮어쓸 때 부른다.
            let title = args
                .first()
                .ok_or_else(|| anyhow!("rename-window needs <title>"))?;
            let surface = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                anyhow!("rename-window needs $KASATERM_PANE_ID (run inside a kasaterm pane)")
            })?;
            (
                "window.rename",
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
        "report-cwd" => {
            // statusline.py 가 매 렌더 호출: report-cwd <surface_id> <cwd> [session_id].
            // claude 내부 cd 를 GUI 푸터 "현재 보는 경로"로 노출.
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("report-cwd needs <surface_id> <cwd> [session_id]"))?;
            let cwd = args.get(1).ok_or_else(|| anyhow!("report-cwd needs a cwd"))?;
            let session_id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            (
                "surface.report_cwd",
                json!({ "surface_id": surface, "cwd": cwd, "session_id": session_id }),
            )
        }
        "split" => {
            // 기본 no-focus(자동화: tell 처럼 포커스 안 뺏음). --focus 로 옵트인.
            let focus = args.iter().any(|a| a == "--focus");
            let dir = args
                .iter()
                .find(|a| !a.starts_with("--"))
                .ok_or_else(|| anyhow!("split needs a direction"))?;
            ("surface.split", json!({ "direction": dir, "focus": focus }))
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
        "resize" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("resize needs <surface_id> <ratio>"))?;
            let ratio: f64 = args
                .get(1)
                .ok_or_else(|| anyhow!("resize needs a ratio (0..1)"))?
                .parse()
                .map_err(|_| anyhow!("ratio must be a number, e.g. 0.6"))?;
            ("surface.set_ratio", json!({ "surface_id": surface, "ratio": ratio }))
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
            // Prepend Ctrl+U (0x15): clear any half-typed line resident in the
            // target prompt. Then wrap in bracketed paste (\x1b[200~…\x1b[201~):
            // claude's Ink input treats this as a safe paste event even in
            // menu/special states where a bare CR was eaten (munder pattern,
            // hiddenClaude.ts). The handler ships body first, then \r 140ms
            // later so Ink has finished processing the paste before the submit.
            let mut params = json!({ "surface_id": surface,
                "text": format!("\x15\x1b[200~{}\x1b[201~\r", flat.trim()) });
            // 발신 메타 동봉 — 서버가 방 기준 slug 의 messages.jsonl 에 기록해 채팅뷰가
            // 학생→학생 tell 을 발신자 좌측 버블로 그린다(거노 #5/#7). CLI 자체 기록은
            // 발신 셸의 cwd 기준 slug 라 cd 상태에 따라 파일이 갈라져 매칭이 새던 것을
            // 서버 기록으로 일원화. PANE_ID 없으면(사람이 직접 친 cli) 거노 발신 = 미기록.
            if let Some(fp) =
                std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty())
            {
                params["from_pane"] = json!(fp);
                params["plain"] = json!(flat.trim());
            }
            ("surface.send_text", params)
        }
        "resume" => {
            // resume <session_id> [cwd] — 사라진(재시작·종료) 학생 세션을 새 pane 에 claude
            // --resume 으로 이어 띄운다(거노: tell 오발송 대신 이어가기). cwd 생략 시 활성 방 cwd.
            let sid = args
                .first()
                .filter(|s| !s.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("resume needs <session_id> [cwd]"))?;
            let cwd = args.get(1).filter(|s| !s.is_empty()).cloned();
            ("session.resume", json!({ "id": sid, "cwd": cwd, "newroom": false }))
        }
        "recent-sessions" => {
            // recent-sessions [cwd] — 이어갈 후보 세션 목록(최신순, id/label/mtime/cwd). tell
            // 오발송(없는 학생) 시 사라진 학생 세션을 찾아 resume 하는 데 쓴다(거노: 내가 자동).
            let cwd = args.first().filter(|s| !s.is_empty()).cloned();
            ("session.recent", json!({ "cwd": cwd }))
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
        "notify" => {
            // notify [--surface <id>] <title> [body...] — fire a "work
            // complete" notification for a pane. A claude Stop hook runs this;
            // --surface defaults to $KASATERM_PANE_ID (the pane it fired in).
            let (surface, rest): (String, &[String]) =
                if args.first().is_some_and(|a| a == "--surface") {
                    let s = args
                        .get(1)
                        .ok_or_else(|| anyhow!("--surface needs an id"))?
                        .clone();
                    (s, args.get(2..).unwrap_or(&[]))
                } else {
                    let s = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                        anyhow!("notify needs --surface <id> or $KASATERM_PANE_ID")
                    })?;
                    (s, &args[..])
                };
            let title = rest
                .first()
                .ok_or_else(|| anyhow!("notify needs a <title>"))?
                .clone();
            let body = rest.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            (
                "surface.notify",
                json!({ "surface_id": surface, "title": title, "body": body }),
            )
        }
        "attention" => {
            // attention [--surface <id>] [reason...] — flag a pane as blocked
            // on a permission / input prompt. A claude `Notification` hook runs
            // this; --surface defaults to $KASATERM_PANE_ID (the pane it fired
            // in). `reason` is free text (the hook's message), optional.
            let (surface, rest): (String, &[String]) =
                if args.first().is_some_and(|a| a == "--surface") {
                    let s = args
                        .get(1)
                        .ok_or_else(|| anyhow!("--surface needs an id"))?
                        .clone();
                    (s, args.get(2..).unwrap_or(&[]))
                } else {
                    let s = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                        anyhow!("attention needs --surface <id> or $KASATERM_PANE_ID")
                    })?;
                    (s, &args[..])
                };
            let reason = rest.join(" ");
            (
                "surface.attention",
                json!({ "surface_id": surface, "reason": reason }),
            )
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

// ---------------------------------------------------------------- sessions --

/// 터미널 세션 피커 본체. `interactive=false`(sessions)면 목록만, `true`(resume)면
/// 번호 입력을 받아 그 세션을 `claude --resume` 으로 이어간다. 목록은
/// `recent_sessions_for`(jsonl 직스캔)라 claude /resume 의 teamName 필터를 안 탄다.
fn run_sessions_picker(interactive: bool, args: &[String]) -> Result<()> {
    let limit = args
        .iter()
        .find_map(|s| s.parse::<usize>().ok())
        .unwrap_or(20);
    let cwd = std::env::current_dir().context("cwd")?;
    let list = kasa_socket::sessions::recent_sessions_for(&cwd, limit);
    if list.is_empty() {
        println!("최근 세션 없음 ({})", cwd.display());
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let bindings =
        read_string_map(&Path::new(&home).join(".config/kasaterm/session_characters.json"));
    let colors = student_colors(&Path::new(&home).join(".config/kasaterm/characters.json"));
    let live = live_session_ids();
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";
    for (i, s) in list.iter().enumerate() {
        let student = bindings.get(&s.id).cloned().unwrap_or_default();
        let color = colors.get(&student).map(|h| ansi_fg(h)).unwrap_or_default();
        let dot = if student.is_empty() { format!("{DIM}·{RESET}") } else { format!("{color}●{RESET}") };
        let name_cell = pad_display(&student, 8);
        let label_cell = pad_display(&clip_display(&s.label, 44), 44);
        let live_mark = if live.contains(&s.id) { " \x1b[31m[실행중]\x1b[0m" } else { "" };
        println!(
            "{:>3}  {dot} {color}{name_cell}{RESET} {label_cell} {DIM}{:>7} · {}{RESET}{live_mark}",
            i + 1,
            rel_time(s.mtime),
            &s.id[..8.min(s.id.len())],
        );
    }
    if !interactive {
        return Ok(());
    }
    print!("\n번호 입력 (Enter=취소): ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).context("stdin")?;
    let Ok(n) = line.trim().parse::<usize>() else {
        println!("취소");
        return Ok(());
    };
    let Some(sel) = n.checked_sub(1).and_then(|i| list.get(i)) else {
        return Err(anyhow!("{n}번 세션이 없어요 (1..{})", list.len()));
    };
    if live.contains(&sel.id) {
        return Err(anyhow!(
            "이미 실행 중인 세션이에요 ({}) — 그 pane 을 쓰거나 `claude agents` 로 attach 하세요. \
             중복 --resume 은 프로세스가 갈라져요.",
            &sel.id[..8]
        ));
    }
    // 사용자 셸(-i)로 실행 — zshrc 의 claude() 래퍼(권한 플래그 등)와 pane PATH 의
    // kasaterm shim(트리플·페르소나)을 사람이 직접 친 것과 똑같이 태운다.
    // id 는 recent_sessions_for 가 uuid 검증을 마친 값이라 인터폴레이션 안전.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let err = std::process::Command::new(shell)
            .arg("-ic")
            .arg(format!("claude --resume {}", sel.id))
            .exec();
        Err(anyhow!("claude 실행 실패: {err}"))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", &format!("claude --resume {}", sel.id)])
            .status()
            .context("claude 실행")?;
        if status.success() { Ok(()) } else { Err(anyhow!("claude exit {status}")) }
    }
}

/// 세션 제목 변경 본체 — `rename [sid|sid8] <이름...>`. sid 생략 시 이 pane 의
/// 세션($KASATERM_RESUMED_SID > $KASATERM_SESSION_ID). claude `/rename` 과 같은
/// custom-title 레코드를 jsonl 에 append 한다 — 라이브 세션도 append-only 라
/// 안전하고, teammate 세션에서 claude 가 "team leader 가 정한다"며 거부하는
/// 것의 공식 우회 경로다. 이름은 남은 인자 전부(공백 포함 가능).
fn run_session_rename(args: &[String]) -> Result<()> {
    use kasa_socket::sessions::{is_uuid, recent_sessions_for, session_jsonl_path};
    let cwd = std::env::current_dir().context("cwd")?;
    let looks_like_sid = |s: &str| {
        is_uuid(s) || (s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
    };
    let (id, name_args): (String, &[String]) = match args.split_first() {
        Some((first, rest)) if is_uuid(first) => (first.clone(), rest),
        Some((first, rest)) if looks_like_sid(first) => {
            // 8자 이상 hex 프리픽스 → 최근 세션에서 유일 매칭만 허용.
            let list = recent_sessions_for(&cwd, 200);
            let hits: Vec<_> = list.iter().filter(|s| s.id.starts_with(first.as_str())).collect();
            match hits.len() {
                1 => (hits[0].id.clone(), rest),
                0 => return Err(anyhow!("{first} 로 시작하는 최근 세션이 없어요")),
                n => return Err(anyhow!("{first} 프리픽스가 세션 {n}개와 겹쳐요 — 더 길게 주세요")),
            }
        }
        _ => {
            let own = std::env::var("KASATERM_RESUMED_SID")
                .ok()
                .filter(|s| is_uuid(s))
                .or_else(|| std::env::var("KASATERM_SESSION_ID").ok().filter(|s| is_uuid(s)))
                .ok_or_else(|| {
                    anyhow!("sid 생략은 kasaterm pane 안에서만 돼요 — rename <sid> <이름>")
                })?;
            (own, args)
        }
    };
    let name = name_args.join(" ").trim().to_string();
    if name.is_empty() {
        return Err(anyhow!("이름이 비었어요 — rename [sid] <이름>"));
    }
    let path = session_jsonl_path(&cwd, &id).ok_or_else(|| anyhow!("$HOME 미설정"))?;
    if !path.exists() {
        return Err(anyhow!("이 cwd 에 세션 transcript 가 없어요: {}", path.display()));
    }
    let record = serde_json::to_string(&json!({
        "type": "custom-title",
        "customTitle": name,
        "sessionId": id,
    }))
    .context("record 직렬화")?;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{record}").context("append")?;
    println!("세션 {} 이름 변경: {name}", &id[..8]);
    Ok(())
}

/// `{sid: 학생명}` 평면 JSON(session_characters.json). 없거나 깨지면 빈 맵.
fn read_string_map(path: &Path) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// characters.json → 학생명 → header_color(#rrggbb). leader/leaders/members 전부.
fn student_colors(path: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(v) = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return out;
    };
    let mut pool: Vec<&Value> = Vec::new();
    if let Some(l) = v.get("leader") {
        pool.push(l);
    }
    for key in ["leaders", "members"] {
        if let Some(arr) = v.get(key).and_then(|a| a.as_array()) {
            pool.extend(arr.iter());
        }
    }
    for m in pool {
        if let (Some(name), Some(color)) = (
            m.get("name").and_then(|n| n.as_str()),
            m.get("header_color").and_then(|c| c.as_str()),
        ) {
            out.insert(name.to_string(), color.to_string());
        }
    }
    out
}

/// `#rrggbb` → 24bit ANSI fg 시퀀스. 파싱 실패면 빈 문자열(무색).
fn ansi_fg(hex: &str) -> String {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return String::new();
    }
    match (
        u8::from_str_radix(&h[0..2], 16),
        u8::from_str_radix(&h[2..4], 16),
        u8::from_str_radix(&h[4..6], 16),
    ) {
        (Ok(r), Ok(g), Ok(b)) => format!("\x1b[38;2;{r};{g};{b}m"),
        _ => String::new(),
    }
}

/// 지금 살아있는 claude 세션 id 들 — ~/.claude/sessions/<pid>.json 레지스트리에서
/// pid 생존(kill -0) 확인. 중복 --resume(프로세스 갈라짐) 방지용.
fn live_session_ids() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = Path::new(&home).join(".claude/sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for e in entries.flatten() {
        let Some(v) = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        else {
            continue;
        };
        let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) else { continue };
        let alive = match v.get("pid").and_then(|p| p.as_u64()) {
            #[cfg(unix)]
            Some(pid) => std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            #[cfg(not(unix))]
            Some(_) => true,
            None => false,
        };
        if alive {
            out.insert(sid.to_string());
        }
    }
    out
}

/// unix secs → "방금/N분 전/N시간 전/N일 전" (arona 웹뷰 피커와 동일 규칙).
fn rel_time(mtime: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let diff = now.saturating_sub(mtime);
    if diff < 60 {
        "방금".into()
    } else if diff < 3600 {
        format!("{}분 전", diff / 60)
    } else if diff < 86400 {
        format!("{}시간 전", diff / 3600)
    } else {
        format!("{}일 전", diff / 86400)
    }
}

/// 터미널 표시폭 — 한글 등 넓은 문자 2칸. 정렬용 근사(동아시아 Wide 전부는 아님).
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if ('\u{1100}'..='\u{FFDC}').contains(&c) { 2 } else { 1 })
        .sum()
}

/// 표시폭 기준으로 자르기(넘치면 … 붙임).
fn clip_display(s: &str, max: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if display_width(&flat) <= max {
        return flat;
    }
    let mut out = String::new();
    let mut w = 0;
    for c in flat.chars() {
        let cw = if ('\u{1100}'..='\u{FFDC}').contains(&c) { 2 } else { 1 };
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// 표시폭 기준 우측 공백 패딩.
fn pad_display(s: &str, width: usize) -> String {
    let w = display_width(s);
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}
