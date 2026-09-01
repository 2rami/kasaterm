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
    // `split --count N` 은 **한 번의 호출로** pane N 개를 배치한다.
    //
    // 예전엔 여기서 split 을 N 번 부르면서 2회차부터 직전에 만든 pane 을 대상으로
    // 삼았다. ⌘D 를 연달아 누른 것과 같은 모양이라 몫이 1/2 → 1/4 → 1/8 로
    // 반감하고, 넷을 부르면 마지막 학생이 화면의 1/16 이었다 — 거노가 매번 드래그로
    // 고쳤다(2026-08-13). 방향을 명시하면 더 나빴다: 모든 회차가 같은 축이라 얇은
    // 세로 기둥 넷이 된다.
    //
    // 서버가 트리를 한 번에 짜므로 왕복도 N 번에서 한 번으로 줄었다. 실패도 부분
    // 성공이 아니라 한 번의 사유로 온다.
    if cmd == "split" {
        let count = args
            .iter()
            .position(|a| a == "--count")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        if count > 1 {
            let from = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()));
            // 호스트 몫을 넘길 수 있게 둔다 — 기본(0.6)은 서버가 정한다.
            let host_ratio = args
                .iter()
                .position(|a| a == "--host-ratio")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse::<f32>().ok());
            let socket_path = resolve_socket_path()?;
            let req = Request {
                id: json!(format!("cli-{}", std::process::id())),
                method: "surface.split_fleet".into(),
                params: json!({ "count": count, "from": from, "host_ratio": host_ratio }),
            };
            let (ok, result, error) = match roundtrip(&socket_path, &req) {
                Ok(r) if r.ok => (true, r.result, None),
                Ok(r) => (
                    false,
                    r.result,
                    Some(r.error.map(|e| e.message).unwrap_or_else(|| "사유 없음".into())),
                ),
                Err(e) => (false, None, Some(format!("{e:#}"))),
            };
            // 요청보다 적게 앉을 수 있다(창 크기 하한). **그 차이를 여기서 말한다** —
            // 개수만 세어 보고 「됐다」로 읽으면 「다섯 불렀는데 셋」이 조용히 지나간다.
            let placed = result
                .as_ref()
                .and_then(|v| v.get("placed"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let short = (ok && placed < count)
                .then(|| format!("창이 좁아 {count} 명 중 {placed} 명만 앉혔다"));
            println!(
                "{}",
                json!({ "ok": ok, "result": result, "error": error, "note": short })
            );
            std::process::exit(if ok { 0 } else { 1 });
        }
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
    // `dismiss <id>…` — 일 끝난 학생 pane 을 한 번에 닫는다. 인사말·완료 보고를
    // 주고받는 대신 오케스트레이터가 그냥 닫는다(회수할 게 있으면 git 이 막는다).
    if cmd == "dismiss" {
        let socket_path = resolve_socket_path()?;
        run_dismiss(&socket_path, &args)?;
        return Ok(None);
    }
    // `closed [%pane]` — 되살리기 목록. pane 을 주면 그 항목을 **진짜 끈다**.
    //
    // 닫은 pane 은 죽지 않는다 — 프로세스를 물고 이 목록에 앉아 있다가 10개를 넘겨
    // 밀려날 때 죽는다. 그래서 `dismiss` 로 정리한 학생 claude 들이 계속 살아 있는데,
    // 그 사실이 GUI 밖에서는 보이지도 않았다(거노 2026-08-06).
    if cmd == "closed" {
        let want = args.iter().find(|a| a.starts_with('%')).cloned();
        let socket_path = resolve_socket_path()?;
        let resp = roundtrip(
            &socket_path,
            &Request {
                id: "closed".into(),
                method: "surface.closed".into(),
                params: match want {
                    Some(p) => json!({ "pane": p }),
                    None => Value::Null,
                },
            },
        )?;
        println!("{}", serde_json::to_string(&resp)?);
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
    // `statusline` — claude statusLine 커맨드(stdin JSON → 한 줄 출력). collab-hooks
    // statusline.py 의 Rust 이식 — Windows 는 python3 가 없어 py 를 못 돌리므로 이
    // 서브커맨드가 pane statusline 을 담당한다(mac 은 검증된 py 경로 유지).
    if cmd == "statusline" {
        run_statusline();
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
    // `activity` 는 사람(과 학생)이 읽는 자리다 — board 처럼 기계가 파싱하는 게
    // 아니라 「쟤 왜 저러나」를 눈으로 훑는 용도라, JSON 대신 시간순 목록으로 낸다.
    if cmd == "activity" && response.ok {
        println!("{}", render_activity(&response));
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
                    let mut status =
                        e.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    // 사용자가 닫아 화면에 없는 pane — 새 일을 시키면 안 보이는
                    // 곳에서 돈다(2026-08-15). 상태에 못 박아 사람도 필터도 잡게.
                    if e.get("detached").and_then(|v| v.as_bool()).unwrap_or(false) {
                        status = format!("{status}·화면밖");
                    }
                    let intent = e.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                    let waiting = e.get("waiting_for").and_then(|v| v.as_str());
                    // 명시적 완료 보고 — 상태 줄에 실어 diff 가 잡게 한다: 보고가
                    // 도착하는 순간(아직 working 이어도) 한 줄이 흐르고, Monitor 의
                    // done 필터가 idle 전에 깨어난다.
                    let done = e.get("done_outcome").and_then(|v| v.as_str());
                    let line = match (done, waiting) {
                        (Some(d), _) => {
                            let sum =
                                e.get("done_summary").and_then(|v| v.as_str()).unwrap_or("");
                            if sum.is_empty() {
                                format!("{status} [done:{d}] — {intent}")
                            } else {
                                format!("{status} [done:{d}] {sum} — {intent}")
                            }
                        }
                        (None, Some(w)) => format!("{status} (waiting: {w}) — {intent}"),
                        (None, None) => format!("{status} — {intent}"),
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
                    let mut status =
                        e.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    // 사용자가 닫아 화면에 없는 pane — 새 일을 시키면 안 보이는
                    // 곳에서 돈다(2026-08-15). 상태에 못 박아 사람도 필터도 잡게.
                    if e.get("detached").and_then(|v| v.as_bool()).unwrap_or(false) {
                        status = format!("{status}·화면밖");
                    }
                    let intent = e.get("intent").and_then(|v| v.as_str()).unwrap_or("");
                    let character = e.get("character").and_then(|v| v.as_str()).unwrap_or("");
                    let who = if character.is_empty() {
                        target.to_string()
                    } else {
                        format!("{character}({target})")
                    };
                    match status.as_str() {
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

/// `dismiss <id>…` — 일이 끝난 학생 pane 을 한 번에 닫는다. 닫기 전에 각 pane 의
/// cwd 를 `git status --porcelain` 으로 보고, **커밋 안 된 변경이 있으면 닫지 않고
/// 보고만** 한다. 회수할 것이 있는지는 오케스트레이터가 pane 을 죽인 뒤엔 물어볼
/// 데가 없기 때문이다 — 워크트리 파일은 남지만 어느 pane 이 무엇을 만지고 있었는지는
/// board 와 함께 사라진다.
///
/// 대상은 **항상 명시**한다(`--all` 없음). 스폰한 쪽은 자기가 띄운 id 를 알고,
/// 화면에는 그 작업과 무관한 pane 이 늘 함께 떠 있다 — 한 번의 오작동이 남의 세션을
/// 통째로 날린다.
///
/// 출력은 JSON 이 아니라 한 줄씩이다. 이 명령을 읽는 것은 사람 아니면 에이전트고,
/// 둘 다 "무엇이 닫혔고 무엇이 남았나" 한 눈에 보는 편이 싸다.
fn run_dismiss(socket_path: &str, args: &[String]) -> Result<()> {
    let force = args.iter().any(|a| a == "--force");
    let targets: Vec<String> = args
        .iter()
        .filter(|a| a.starts_with('%'))
        .cloned()
        .collect();
    if targets.is_empty() {
        return Err(anyhow!(
            "dismiss 는 닫을 pane 을 명시해야 한다 (예: dismiss %3 %4 [--force])"
        ));
    }
    let board = roundtrip(
        socket_path,
        &Request {
            id: "dismiss".into(),
            method: "collab.board".into(),
            params: json!({}),
        },
    )
    .ok()
    .and_then(|r| r.result)
    .and_then(|v| v.get("board").cloned())
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    // board 는 transcript 가 바인딩된 pane 만 싣는다 — codex pane 이나 셸뿐인 pane 은
    // 줄이 없다. 그때 학생·cwd 를 여기서 보충하지 않으면 `? — ` 만 찍히고, 더 나쁘게는
    // cwd 를 몰라 **커밋 안 된 변경 검사가 통째로 건너뛰어진다**(이 명령의 존재 이유다).
    let surfaces = roundtrip(
        socket_path,
        &Request {
            id: "dismiss".into(),
            method: "surface.list".into(),
            params: json!({}),
        },
    )
    .ok()
    .and_then(|r| r.result)
    .and_then(|v| v.get("surfaces").cloned())
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    let mut kept = 0usize;
    for id in &targets {
        let entry = board
            .iter()
            .find(|e| e.get("surface_id").and_then(|v| v.as_str()) == Some(id.as_str()));
        let surf = surfaces
            .iter()
            .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id.as_str()));
        let pick = |key: &str, alt: &str| -> Option<String> {
            entry
                .and_then(|e| e.get(key))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| surf.and_then(|e| e.get(alt)).and_then(|v| v.as_str()))
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let who = pick("character", "character").unwrap_or_else(|| "?".into());
        let who = who.as_str();
        let cwd = pick("cwd", "cwd").unwrap_or_default();
        let cwd = cwd.as_str();
        let where_ = Path::new(cwd)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| cwd.to_string());
        let dirty = if force || cwd.is_empty() {
            0
        } else {
            git_dirty_count(cwd)
        };
        if dirty > 0 {
            kept += 1;
            println!("kept    {id} {who} — {where_}: 커밋 안 된 변경 {dirty}개");
            continue;
        }
        let resp = roundtrip(
            socket_path,
            &Request {
                id: "dismiss".into(),
                method: "surface.close".into(),
                params: json!({ "surface_id": id }),
            },
        )?;
        if resp.ok {
            println!("closed  {id} {who} — {where_}");
        } else {
            kept += 1;
            let why = resp
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "close 실패".into());
            println!("failed  {id} {who} — {why}");
        }
    }
    if kept > 0 {
        println!("\n남은 {kept}개는 손대지 않았다 — 회수하거나 --force 로 다시.");
    }
    Ok(())
}

/// 그 디렉토리의 커밋 안 된 변경 개수. git 이 아니거나 실패하면 0 — "모르면 닫는다"
/// 가 아니라 "모르면 막지 않는다" 쪽인데, 여기서 막으면 git 아닌 cwd 의 pane 을
/// 영영 못 닫는다. 진짜 회수 대상은 워크트리이고 그건 git 이다.
fn git_dirty_count(cwd: &str) -> usize {
    std::process::Command::new("git")
        .args(["-C", cwd, "--no-optional-locks", "status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0)
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
    kasa_socket::collab_root()
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
/// `activity` 응답을 시간순 목록으로. 라벨은 네 글자로 맞춰 왼쪽 기둥이 서게 한다 —
/// 도구와 결과가 번갈아 오므로 기둥이 없으면 어디까지가 한 동작인지 안 보인다.
fn render_activity(resp: &Response) -> String {
    let events = resp
        .result
        .as_ref()
        .and_then(|r| r.get("events"))
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    if events.is_empty() {
        return "활동 없음 — 이 pane 의 transcript 꼬리에 도구 호출이 없다.".to_string();
    }
    let mut out = format!("최근 활동 {}건 (오래된 것부터)\n", events.len());
    for e in &events {
        let g = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let kind = g("kind");
        let err = e.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
        let label = match kind {
            "prompt" => "시킴",
            "say" => "말함",
            "tool" => "도구",
            _ if err => "오류",
            _ => "결과",
        };
        let name = g("name");
        let head = if name.is_empty() {
            String::new()
        } else {
            format!("{name} ")
        };
        // 도구 인자는 JSON 원문이라 줄바꿈이 `\n` 두 글자로 남는다 — 여러 줄 셸
        // 명령이 한 줄로 뭉쳐 읽을 수 없으므로 여기서 푼다(소켓 응답 쪽은 기계가
        // 파싱하는 자리라 원문 그대로 둔다). 이어지는 줄은 기둥 폭만큼 들여써
        // 한 동작으로 묶어 보이게 한다.
        let body = g("text").replace("\\n", "\n").replace('\n', "\n            ");
        out.push_str(&format!("  {label}  {head}{body}\n"));
    }
    out
}

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
        // ⚠️ 순서가 계약이다. 전에는 `.min(W - 1).max(x0 + 2)` 였는데, `max` 가
        // 상한을 **덮어써서** 아주 좁은 pane(x0 이 오른쪽 끝에 붙은 경우) 하나로
        // `bits[..][W]` 를 짚고 패닉했다 — 방이 많아 pane 이 잘게 쪼개지면 재현된다
        // (2026-08-28 실측: 창 14개에서 `windows` 가 통째로 죽었다).
        // x0 을 먼저 묶어 두면 `x0 + 2` 가 상한을 넘을 수 없다.
        let x0 = cx(*x).min(W - 3);
        let y0 = cy(*y).min(H - 3);
        let x1 = cx(x + w).clamp(x0 + 2, W - 1);
        let y1 = cy(y + h).clamp(y0 + 2, H - 1);
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
    eprintln!(
        "  kasaterm-cli dismiss <surface_id>… [--force]  # 일 끝난 학생 pane 닫기(커밋 안 된 변경이 있으면 안 닫고 보고)
  kasaterm-cli closed [%pane]                # 되살리기 목록(닫아도 안 죽은 pane 들). %pane 을 주면 그것만 진짜 끈다"
    );
    eprintln!("  kasaterm-cli rename <surface_id> <title>");
    eprintln!("  kasaterm-cli rename-window <title>          # 이 pane 의 세션 이름");
    eprintln!("  kasaterm-cli color <surface_id> <#rrggbb>");
    eprintln!(
        "  kasaterm-cli split <left|right|up|down> [%surface] [--focus] [--count N] [--host-ratio 0.6]  # 기본 no-focus·이 pane 을 쪼갬. --count N 은 부른 쪽을 크게 두고 N 명을 균등하게 배치(몫이 반감하지 않는다). 창이 좁으면 앉힌 인원이 요청보다 적고 note 에 적힌다"
    );
    eprintln!("  kasaterm-cli window-new                    # 새 창
  kasaterm-cli web   <url> [%surface]        # URL 을 그 pane 옆 웹(브라우저) pane 으로 (기본: 이 pane 옆)
  kasaterm-cli web-text  [%surface]          # 웹 pane 본문 읽기 (innerText). %surface 생략 = 웹 pane 이 하나일 때
  kasaterm-cli web-eval  '<js>' [%surface]   # 웹 pane 에서 JS 실행, 결과를 JSON 으로 (클릭·입력·검사 전부 이것으로)
  kasaterm-cli web-shot  </abs/x.png> [%surface]  # 웹 pane 스크린샷을 파일로 (창에 이미지 안 실림)
  kasaterm-cli web-url   [%surface]          # 웹 pane 의 현재 주소
  kasaterm-cli promote <%surface>            # 도는 pane 을 로컬 상주 데몬으로 무중단 승격 — 앱을 굽고 껐다 켜도 그 학생은 안 죽는다
  kasaterm-cli migrate [%surface] <기계이름|http://호스트:포트|local> [--cwd /레포] [--force]  # pane 의 claude 를 그 기계로 이사(대화·미커밋 변경까지 운반+같은 자리 재개). 기계이름(예: 맥미니)이면 주소·경로를 명부(machines.json)에서 알아서 정한다. %surface 를 빼면 **이 명령을 친 pane 자신**이 간다 — 학생이 자기 이사를 신청하는 길. `local` 이면 역이사: 원격 pane 을 이 기계로 데려온다
  kasaterm-cli unfold <라벨>                  # 기계의 학생 pane 전부를 거울로 펼침
  kasaterm-cli remote <http://호스트:포트> [--cwd /원격/경로] [--attach web-id] [%surface]  # 원격 PTY 호스트(kasa-serve-web)의 셸을 pane 으로 — 앱을 꺼도 원격 셸은 산다
  kasaterm-cli tab   [%surface] [--focus]    # 쪼개지 않고 이 pane 안에 새 탭(화면이 안 줄어든다). 서브에이전트는 여기에 — 응답의 agent 로 바로 SendMessage. --focus 만 탭을 앞으로
  kasaterm-cli move  <surface> <target> [left|right|up|down]  # 대상이 다른 창이면 창을 건너뛴다(PTY 유지)
  kasaterm-cli swap  <surface_a> <surface_b>");
    eprintln!("  kasaterm-cli resize <surface_id> <ratio>   # 직계 split 에서 차지 비중 0..1 (오케스트레이터 크게)");
    eprintln!("  kasaterm-cli send  <text>");
    eprintln!("  kasaterm-cli send  --surface <id> <text>");
    eprintln!("  kasaterm-cli key   [--surface <id>] <enter|tab|escape|up|down|left|right|...>  # 특정 pane에 키/선택");
    eprintln!("  kasaterm-cli tell  [--force] <surface_id> <text>  # send + submit (codex 등 SendMessage 밖 전용 — claude pane 은 거부, 비상시 --force)");
    eprintln!("  kasaterm-cli board [screen_lines]         # what every pane is doing (+ screen tail if N given)");
    eprintln!("  kasaterm-cli board-watch [interval_s]     # stream changed pane status (1 line/change) — feed a Claude Code Monitor");
    eprintln!("  kasaterm-cli wake-watch <surface_id> [interval_s] [--timeout s]  # block until a teammate finishes one turn, then exit (run as a background task → auto-wakes you)");
    eprintln!("  kasaterm-cli layout                       # where each pane sits (active window, %)");
    eprintln!("  kasaterm-cli windows                      # every window (sidebar order) + its panes");
    eprintln!("  kasaterm-cli peek  [surface_id] [lines]   # read a pane's visible screen
  kasaterm-cli capture [surface_id] [path] [--max-width N]
                                            # screenshot ONE pane to PNG (peek's picture twin)
  kasaterm-cli capture --window [path] [--max-width N]
                                            # the WHOLE window incl. sidebar/tabs/columns (main window only)");
    eprintln!("  kasaterm-cli transcript [surface_id] [N]  # last N turns (prompts+replies) of a pane's claude");
    eprintln!("  kasaterm-cli activity [surface_id] [N]    # 그 pane 이 실제로 한 일 — 도구·인자·결과를 시간순으로 (왜 저러나)");
    eprintln!("  kasaterm-cli bind-transcript <path>       # register THIS pane's claude transcript (hook)");
    eprintln!("  kasaterm-cli notify [--surface <id>] <title> [body]  # fire a work-complete notification (Stop hook)");
    eprintln!("  kasaterm-cli attention [--surface <id>] [reason]     # flag a pane blocked on a permission/input prompt (Notification hook)");
    eprintln!("  kasaterm-cli done [--surface <id>] <succeeded|failed> [한 줄 요약]  # 브리프 완료 보고 — board 가 idle 추정 대신 이걸 정본으로 싣는다");
    eprintln!("  kasaterm-cli agent-status <start|end|clear> <subagent|background> [key] [라벨]  # 진행 표시 정본(PreToolUse/PostToolUse 훅)");
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
            // ⚠️ 남는 인자를 조용히 버리지 않는다. 이름이 `rename` 과 닮아
            // `rename-window %21 "이름"` 처럼 pane 을 앞에 붙여 부르는 실수가 나는데,
            // 그러면 첫 인자가 제목으로 먹혀 **방 이름이 `%21` 이 된다**(2026-08-25
            // 실측: 방 4 가 그렇게 불리고 있었다). 따옴표를 빠뜨려 제목이 두 토막
            // 난 경우도 여기 걸린다 — 앞 토막만 먹고 마는 것보다 알려주는 게 낫다.
            let pane_like = |s: &str| {
                s.strip_prefix('%')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            };
            if args.len() > 1 || args.first().is_some_and(|t| pane_like(t)) {
                return Err(anyhow!(
                    "rename-window 는 제목 하나만 받는다(대상은 이 pane 이 속한 방)\n\
                     - pane 제목을 바꾸려면: kasaterm-cli rename <surface_id> <title>\n\
                     - 제목에 공백이 있으면 따옴표로 묶어라"
                ));
            }
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
        "repersona" => {
            // 이 pane 의 다음 claude 가 쓸 캐릭터를 갈아끼운다(respawn 없음). 이름은
            // 활성 테마 밖이어도 된다 — 설치 테마까지 합쳐 찾으므로, 나쵸 전용 테마를
            // 깔아 두고 그 pane 에서만 부르는 쓰임이 여기다.
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("repersona needs <surface_id> <character>"))?;
            let character = args
                .get(1)
                .ok_or_else(|| anyhow!("repersona needs a character name"))?;
            (
                "surface.repersona",
                json!({ "surface_id": surface, "character": character }),
            )
        }
        "report-cwd" => {
            // statusline.py 가 매 렌더 호출:
            //   report-cwd <surface_id> <cwd> [session_id] [ctx_window] [ctx_tokens] [model] [effort]
            // claude 내부 cd 를 GUI 푸터 "현재 보는 경로"로 노출하고, 컨텍스트 창·사용
            // 토큰을 함께 실어 board 의 ctx% 분모를 확정한다(추정 대신 하네스 정본).
            // 뒤 넷은 선택 — 구버전 statusline 은 안 보내고, 그때는 0/빈값이라 GUI 가 폴백한다.
            //
            // model 은 훅 stdin 의 `model.id` **원본**이다(`claude-opus-5[1m]`). 재시작 뒤
            // 같은 모델로 되살리는 데 쓰므로 `[1m]` 이 붙은 채로 와야 한다 — board 에 뜨는
            // 쪽은 API 응답 표기라 그걸 되먹이면 1M 세션이 200k 로 강등된다.
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("report-cwd needs <surface_id> <cwd> [session_id]"))?;
            let cwd = args.get(1).ok_or_else(|| anyhow!("report-cwd needs a cwd"))?;
            let session_id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let ctx_window: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let ctx_tokens: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let model = args.get(5).map(|s| s.as_str()).unwrap_or("");
            let effort = args.get(6).map(|s| s.as_str()).unwrap_or("");
            (
                "surface.report_cwd",
                json!({
                    "surface_id": surface,
                    "cwd": cwd,
                    "session_id": session_id,
                    "ctx_window": ctx_window,
                    "ctx_tokens": ctx_tokens,
                    "model": model,
                    "effort": effort,
                }),
            )
        }
        "split" => {
            // 기본 no-focus(자동화: tell 처럼 포커스 안 뺏음). --focus 로 옵트인.
            let focus = args.iter().any(|a| a == "--focus");
            // 방향은 **선택**이다 — 생략하면 `auto`, 즉 앱이 pane 의 종횡비를 보고 긴
            // 축을 쪼갠다(거노 2026-08-05: "너무 가로로나 세로로 안 길게"). 사람이
            // 방향을 정해 부를 때만 명시하면 된다.
            let dir = args
                .iter()
                .find(|a| !a.starts_with("--") && !a.starts_with('%'))
                .map(String::as_str)
                .unwrap_or("auto");
            // 쪼갤 pane: 명시한 %id > 이 CLI 가 도는 pane. 둘 다 없을 때만(=pane 밖에서
            // 부른 경우) 포커스 기준으로 떨어진다. 예전엔 늘 포커스 기준이라, 에이전트가
            // 자기 pane 에서 학생을 띄워도 사람이 보고 있는 창이 쪼개졌다.
            let from = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()));
            (
                "surface.split",
                json!({ "direction": dir, "focus": focus, "from": from }),
            )
        }
        // 새 창(사이드바에 하나 더). 창 간 이동(`move`)의 목적지를 만들 때 쓴다.
        "window-new" => ("window.new", json!({})),
        // 도는 pane 을 로컬 상주 데몬으로 **무중단 승격** — 셸·claude 는 그대로,
        // 소유권만 앱 밖으로. 이후 앱을 굽고 껐다 켜도 그 학생은 안 죽는다.
        "promote" => {
            let pane = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()))
                .ok_or_else(|| anyhow!("promote 는 대상 pane 이 필요해요 (예: promote %3)"))?;
            ("surface.promote", json!({ "pane": pane }))
        }
        // pane 의 claude 를 **다른 기계로 이사** — 대화를 통째 그 호스트로 옮기고
        // 같은 자리에서 같은 대화로 다시 깨운다. 안 올린 git 변경이 있으면 막아 선다.
        "migrate" => {
            let mut positional: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                let a = &args[i];
                if a == "--cwd" || a == "--run" {
                    i += 2;
                    continue;
                }
                if a.starts_with('%') || a.starts_with("--") {
                    i += 1;
                    continue;
                }
                positional.push(a.clone());
                i += 1;
            }
            let base = positional.first().cloned().ok_or_else(|| {
                anyhow!("migrate 는 목적지가 필요해요 (예: migrate 맥미니 · migrate %3 맥미니 · 데려오기: migrate %3 local)")
            })?;
            let flagval = |name: &str| {
                args.iter()
                    .position(|a| a == name)
                    .and_then(|i| args.get(i + 1))
                    .cloned()
            };
            let pane = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()))
                .ok_or_else(|| anyhow!("migrate 는 대상 pane 이 필요해요 (예: migrate %3 http://...)"))?;
            (
                "surface.migrate",
                json!({
                    "pane": pane,
                    "base": base,
                    "cwd": flagval("--cwd"),
                    "run": flagval("--run"),
                    "force": args.iter().any(|a| a == "--force"),
                }),
            )
        }
        // 기계 라벨 하나로 그 기계 학생 pane 전부를 거울로 펼친다.
        "unfold" => {
            let label = args
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("unfold 는 기계 라벨이 필요해요 (예: unfold 맥미니)"))?;
            ("machine.unfold", json!({ "label": label }))
        }
        // 원격 PTY 호스트(kasa-serve-web)의 셸을 pane 으로 — 학생을 맥미니에서
        // 돌리고 이 창은 미러다. 앱을 꺼도(detach) 원격 셸은 살아남고, 재시작하면
        // 같은 세션에 다시 붙는다.
        "remote" => {
            // 플래그 값(--cwd /x)이 base 로 오인되지 않게 위치 인자만 걷는다.
            let mut positional: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                let a = &args[i];
                if a == "--cwd" || a == "--attach" {
                    i += 2;
                    continue;
                }
                if a.starts_with('%') || a.starts_with("--") {
                    i += 1;
                    continue;
                }
                positional.push(a.clone());
                i += 1;
            }
            let base = positional.first().cloned().ok_or_else(|| {
                anyhow!("remote 는 호스트 주소가 필요해요 (예: remote http://127.0.0.1:18766 --cwd /Users/miku)")
            })?;
            let flagval = |name: &str| {
                args.iter()
                    .position(|a| a == name)
                    .and_then(|i| args.get(i + 1))
                    .cloned()
            };
            let from = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()));
            (
                "surface.remote",
                json!({ "base": base, "cwd": flagval("--cwd"), "pane": flagval("--attach"), "from": from }),
            )
        }
        // 쪼개지 않고 **이 pane 안에** 새 탭. 학생을 더 띄워도 화면이 안 줄어든다.
        // 기본은 no-focus — 부모(부른 쪽) 화면이 그대로 남는다. --focus 만 새 탭을
        // 앞으로 올린다(split 의 --focus 와 같은 규약).
        "tab" => {
            let focus = args.iter().any(|a| a == "--focus");
            let outer = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()));
            ("surface.new_tab", json!({ "outer": outer, "focus": focus }))
        }
        // URL 을 요청 pane 옆 웹(브라우저) pane 으로. 개발 서버를 그 서버를
        // 띄운 pane 곁에 두는 용도 — 어느 방 어느 pane 이 띄운 건지 화면
        // 배치가 말해 준다.
        "web" => {
            let url = args
                .iter()
                .find(|a| !a.starts_with('%'))
                .ok_or_else(|| anyhow!("web needs a URL (e.g. web localhost:5173)"))?;
            let target = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()));
            (
                "surface.open_preview",
                json!({ "kind": "web", "path": url, "target": target }),
            )
        }
        // 웹 pane 조종 — 열어 둔 내장 브라우저를 확인 도구로 쓴다. %surface 를
        // 안 주면 열린 웹 pane 이 하나일 때만 그걸 잡는다(여럿이면 후보 나열
        // 오류). eval 결과는 JSON 직렬화 문자열이다.
        "web-eval" | "web-text" | "web-shot" | "web-url" => {
            let op = &cmd[4..]; // "web-eval" → "eval"
            let surface = args.iter().find(|a| a.starts_with('%')).cloned();
            let arg = args.iter().find(|a| !a.starts_with('%')).cloned();
            match (op, &arg) {
                ("eval", None) => anyhow::bail!("web-eval needs JS (e.g. web-eval 'document.title')"),
                ("shot", Some(p)) if !p.starts_with('/') => {
                    anyhow::bail!("web-shot needs an absolute path (got {p})")
                }
                ("shot", None) => anyhow::bail!("web-shot needs an absolute .png path"),
                _ => {}
            }
            (
                "web.drive",
                json!({ "op": op, "arg": arg.unwrap_or_default(), "surface": surface }),
            )
        }
        // pane 을 다른 pane 옆으로 — 대상이 다른 창이면 **창을 건너뛴다**(PTY 유지).
        "move" => {
            let moving = args
                .first()
                .ok_or_else(|| anyhow!("move needs <surface> <target> [left|right|up|down]"))?;
            let target = args
                .get(1)
                .ok_or_else(|| anyhow!("move needs a target surface to land beside"))?;
            let dir = args.get(2).map(String::as_str).unwrap_or("right");
            (
                "surface.move",
                json!({ "surface_id": moving, "target": target, "direction": dir }),
            )
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
            // SendMessage 로 닿는 claude pane 은 서버가 거부한다(SM·tell 이중 발송
            // 차단) — 인박스가 정말 죽었을 때만 --force 로 강행.
            let force = args.first().is_some_and(|a| a == "--force");
            let args = if force { &args[1..] } else { &args[..] };
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
            // 발신 학생 마커 — 받는 pane 이 tell 을 발신자 테마색으로 렌더하려면 화면에
            // 앵커가 필요하다(터미널은 그리드라 transcript 대조로 user 턴을 못 집는다).
            // 발신 pane 자기 캐릭터($KASATERM_CHARACTER)를 `⟦이름⟧` 로 앞에 심는다 —
            // 사람이 직접 친 cli 는 env 가 없어 마커 없이(거노 발신=무색) 나간다.
            let marked = match std::env::var("KASATERM_CHARACTER")
                .ok()
                .filter(|s| !s.is_empty())
            {
                Some(c) => format!("⟦{c}⟧ {}", flat.trim()),
                None => flat.trim().to_string(),
            };
            let mut params = json!({ "surface_id": surface,
                "text": format!("\x15\x1b[200~{}\x1b[201~\r", marked) });
            // 발신 메타 동봉 — 서버가 방 기준 slug 의 messages.jsonl 에 기록해 채팅뷰가
            // 학생→학생 tell 을 발신자 좌측 버블로 그린다(거노 #5/#7). CLI 자체 기록은
            // 발신 셸의 cwd 기준 slug 라 cd 상태에 따라 파일이 갈라져 매칭이 새던 것을
            // 서버 기록으로 일원화. PANE_ID 없으면(사람이 직접 친 cli) 거노 발신 = 미기록.
            if let Some(fp) =
                std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty())
            {
                params["from_pane"] = json!(fp);
                // plain 도 마커 포함 — 웹뷰 senderOf 가 transcript 정확대조라 마커가
                // 한쪽에만 있으면 발신자 버블 매칭이 깨진다.
                params["plain"] = json!(marked);
                if force {
                    params["force"] = json!(true);
                }
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
        "agent-status" => {
            // agent-status [--surface <id>] <start|end|clear> <subagent|background> [key] [라벨...]
            //
            // 진행 표시의 정본. `PreToolUse`/`PostToolUse` 훅이 부르며, 화면이나
            // transcript 를 되짚지 않고 **일어난 그 순간** 사실을 밀어 넣는다.
            // 옛 방식(꼬리 64KB 에서 런치·회수 짝짓기)은 세션이 커지면 런치가 창
            // 밖으로 밀려 오래 걸리는 작업일수록 안 보였다.
            //
            // 훅에서 부르는 것이라 **실패해도 조용해야 한다** — 이 명령이 죽어서
            // claude 의 도구 호출이 막히면 안 된다(호출부가 `|| true` 로 감싼다).
            let (surface, rest): (String, &[String]) =
                if args.first().is_some_and(|a| a == "--surface") {
                    let s = args
                        .get(1)
                        .ok_or_else(|| anyhow!("--surface needs an id"))?
                        .clone();
                    (s, args.get(2..).unwrap_or(&[]))
                } else {
                    let s = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                        anyhow!("agent-status needs --surface <id> or $KASATERM_PANE_ID")
                    })?;
                    (s, &args[..])
                };
            let phase = rest
                .first()
                .ok_or_else(|| anyhow!("agent-status needs <start|end|clear>"))?
                .clone();
            let kind = rest
                .get(1)
                .ok_or_else(|| anyhow!("agent-status needs <subagent|background>"))?
                .clone();
            let key = rest.get(2).cloned().unwrap_or_default();
            let label = rest.get(3..).map(|s| s.join(" ")).unwrap_or_default();
            (
                "surface.agent_status",
                json!({
                    "surface_id": surface,
                    "phase": phase,
                    "kind": kind,
                    "key": key,
                    "label": label,
                }),
            )
        }
        "done" => {
            // done [--surface <id>] <succeeded|failed> [요약...] — 브리프를 마친
            // 학생의 명시적 완료 보고. 오케스트레이터가 board 에서 완료를 추정하지
            // 않고 읽게 한다. --surface 기본값은 자기 pane($KASATERM_PANE_ID).
            let (surface, rest): (String, &[String]) =
                if args.first().is_some_and(|a| a == "--surface") {
                    let s = args
                        .get(1)
                        .ok_or_else(|| anyhow!("--surface needs an id"))?
                        .clone();
                    (s, args.get(2..).unwrap_or(&[]))
                } else {
                    let s = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                        anyhow!("done needs --surface <id> or $KASATERM_PANE_ID")
                    })?;
                    (s, &args[..])
                };
            let outcome = match rest.first().map(String::as_str) {
                // 흔한 이형 표기는 여기서 정규형으로 — 서버는 두 값만 받는다.
                Some("succeeded" | "success" | "ok") => "succeeded",
                Some("failed" | "fail") => "failed",
                Some(other) => {
                    return Err(anyhow!(
                        "done outcome must be succeeded|failed, got \"{other}\""
                    ))
                }
                None => return Err(anyhow!("done needs <succeeded|failed> [한 줄 요약]")),
            };
            let summary = rest.get(1..).unwrap_or(&[]).join(" ");
            (
                "surface.done",
                json!({ "surface_id": surface, "outcome": outcome, "summary": summary }),
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
        "capture" => {
            // peek 의 그림 짝. 텍스트로는 안 보이는 것(색·정렬·겹침)을 판정하려면
            // 화면 자체가 필요하다 — 결과 경로를 Read 로 열면 된다.
            //   capture [surface_id] [path] [--max-width N]
            //   capture --window [path] [--max-width N]
            //
            // `--window` 는 pane 이 아니라 **창 한 장**이다. pane 만 찍어서는 사이드바·
            // 탭바·우측 칼럼이 안 보여, 에이전트가 제가 만든 UI 를 확인할 수 없다.
            // 신호는 **빈 surface_id** — GUI 쪽(`arm_pane_capture`)이 그때 크롭을 안 세운다.
            let mut positional: Vec<String> = Vec::new();
            let mut max_width: Option<u64> = None;
            let mut whole_window = false;
            let mut it = args.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--window" => whole_window = true,
                    "--max-width" | "-w" => {
                        max_width = it.next().and_then(|s| s.parse().ok());
                    }
                    s if s.starts_with("--max-width=") => {
                        max_width = s.split_once('=').and_then(|(_, v)| v.parse().ok());
                    }
                    s => positional.push(s.to_string()),
                }
            }
            let surface = if whole_window {
                String::new()
            } else {
                positional.first().cloned().or_else(|| std::env::var("KASATERM_PANE_ID").ok()).ok_or_else(
                    || anyhow!("capture needs a surface_id (or $KASATERM_PANE_ID) — or --window for the whole window"),
                )?
            };
            let mut params = json!({ "surface_id": surface });
            // `--window` 면 pane 자리가 없으니 경로가 첫 위치 인자다.
            if let Some(p) = positional.get(usize::from(!whole_window)) {
                params["path"] = json!(p);
            }
            if let Some(w) = max_width {
                params["max_width"] = json!(w);
            }
            ("surface.capture", params)
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
        "activity" => {
            // 형제 pane 이 **실제로 무엇을 했나** — 부른 도구, 그 인자, 돌아온 결과를
            // 시간순으로. `transcript` 는 대화만(도구를 버린다), `board` 는 도구 라벨을
            // 짧게 잘라 여덟 개만 준다. 「쟤 뭐 하나」는 board, 「쟤 왜 저러나」는 이쪽.
            //   activity [surface_id] [N]
            let surface = args
                .first()
                .cloned()
                .or_else(|| std::env::var("KASATERM_PANE_ID").ok())
                .ok_or_else(|| anyhow!("activity needs a surface_id (or $KASATERM_PANE_ID)"))?;
            let mut params = json!({ "surface_id": surface });
            if let Some(n) = args.get(1).and_then(|s| s.parse::<u64>().ok()) {
                params["limit"] = json!(n);
            }
            ("collab.activity", params)
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
/// 번호 입력을 받아 그 세션을 이어간다. 목록은 jsonl·SQLite 직스캔이라 claude
/// /resume 의 teamName 필터를 안 탄다.
///
/// 기본은 **세 하네스 전체**(claude·codex·agy)를 가로지른다 — 하네스가 셋이 된
/// 뒤로 "어디서 뭘 하다 말았나"를 한 자리에서 봐야 하기 때문이다. `--here` 는
/// 예전처럼 지금 cwd 의 claude 세션만 본다(그 프로젝트 것만 훑을 때).
fn run_sessions_picker(interactive: bool, args: &[String]) -> Result<()> {
    let limit = args
        .iter()
        .find_map(|s| s.parse::<usize>().ok())
        .unwrap_or(20);
    let here = args.iter().any(|a| a == "--here");
    // 하네스를 대놓고 고를 수 있어야 한다. 합친 목록은 최신순이라 요즘 안 쓰는
    // 쪽(예: codex)이 통째로 뒤로 밀리는데, 그걸 찾자고 limit 를 키우면 화면이
    // 다른 하네스로 덮인다.
    let only = args
        .iter()
        .find(|a| matches!(a.as_str(), "claude" | "codex" | "agy"))
        .cloned();
    let cwd = std::env::current_dir().context("cwd")?;
    let list = match only.as_deref() {
        Some("claude") if here => kasa_socket::sessions::recent_sessions_for(&cwd, limit),
        Some("claude") => kasa_socket::sessions::recent_claude_sessions_all(limit),
        Some("codex") if here => kasa_socket::sessions::recent_codex_sessions_for(&cwd, limit),
        Some("codex") => kasa_socket::sessions::recent_codex_sessions(limit),
        Some("agy") => kasa_socket::sessions::recent_agy_sessions(limit),
        // 하네스를 안 고른 `--here` 는 세 하네스를 가로지른다 — 예전엔 claude 만
        // 봐서, 이 폴더에서 codex 로 일한 기록이 목록에 없는 것이 됐다.
        _ if here => kasa_socket::sessions::recent_sessions_here(&cwd, limit),
        _ => kasa_socket::sessions::recent_all_sessions(limit),
    };
    if list.is_empty() {
        if here {
            println!("최근 세션 없음 ({})", cwd.display());
        } else {
            println!("최근 세션 없음");
        }
        return Ok(());
    }
    let home = kasa_socket::home_dir().unwrap_or_default();
    let bindings =
        read_string_map(&home.join(".config/kasaterm/session_characters.json"));
    let colors = student_colors(&home.join(".config/kasaterm/characters.json"));
    let live = live_session_ids();
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";
    for (i, s) in list.iter().enumerate() {
        let student = bindings.get(&s.id).cloned().unwrap_or_default();
        let color = colors.get(&student).map(|h| ansi_fg(h)).unwrap_or_default();
        let dot = if student.is_empty() { format!("{DIM}·{RESET}") } else { format!("{color}●{RESET}") };
        let name_cell = pad_display(&student, 8);
        let label_cell = pad_display(&clip_display(&s.label, 38), 38);
        // 실행 중 표시는 claude 세션 id 로만 판정된다(live_session_ids). 다른
        // 하네스에 그 잣대를 대면 늘 "아님"이라 거짓 안심을 준다 — 아예 안 붙인다.
        let live_mark = if s.harness == "claude" && live.contains(&s.id) {
            " \x1b[31m[실행중]\x1b[0m"
        } else {
            ""
        };
        // 위치는 프로젝트 이름이 제일 쓸모 있다. cwd 를 모르는 하네스(codex 등)는
        // short id 로 대신한다 — 목록에서 같은 제목을 가릴 최소한의 단서.
        let where_cell = std::path::Path::new(&s.cwd)
            .file_name()
            .and_then(|x| x.to_str())
            .map(|x| x.to_string())
            .unwrap_or_else(|| s.id.chars().take(8).collect());
        println!(
            "{:>3}  {dot} {color}{name_cell}{RESET} {label_cell} {DIM}{:<6} {:>7} · {}{RESET}{live_mark}",
            i + 1,
            s.harness,
            rel_time(s.mtime),
            clip_display(&where_cell, 18),
        );
        // 제목은 세션이 무엇으로 시작했나일 뿐이다. 어디서 멈췄는지는 이 줄에만
        // 있고, 그게 스무 개 중 하나를 고르는 근거가 된다. 없으면 안 그린다 —
        // 빈 들여쓰기 줄이 목록 높이만 두 배로 만든다.
        if !s.preview.is_empty() {
            println!("     {DIM}{}{RESET}", clip_display(&s.preview, term_cols().saturating_sub(6)));
        }
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
    if sel.harness == "claude" && live.contains(&sel.id) {
        return Err(anyhow!(
            "이미 실행 중인 세션이에요 ({}) — 그 pane 을 쓰거나 `claude agents` 로 attach 하세요. \
             중복 --resume 은 프로세스가 갈라져요.",
            &sel.id[..8]
        ));
    }
    let run = kasa_socket::sessions::resume_command(&sel.harness, &sel.id, &sel.cwd);
    // 사용자 셸(-i)로 실행 — zshrc 의 claude() 래퍼(권한 플래그 등)와 pane PATH 의
    // kasaterm shim(트리플·페르소나)을 사람이 직접 친 것과 똑같이 태운다.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let err = std::process::Command::new(shell).arg("-ic").arg(&run).exec();
        Err(anyhow!("{} 실행 실패: {err}", sel.harness))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", &run])
            .status()
            .context("하네스 실행")?;
        if status.success() { Ok(()) } else { Err(anyhow!("exit {status}")) }
    }
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
    let home = kasa_socket::home_dir().unwrap_or_default();
    let dir = home.join(".claude/sessions");
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
/// 터미널 가로 칸 수. 못 알아내면 80 으로 본다.
///
/// 접히면 목록이 통째로 망가진다 — 한 항목이 두 줄이 되면서 번호와 내용이
/// 어긋나 무엇을 고르는지 알 수 없게 된다. 그래서 넘치게 두느니 자른다.
fn term_cols() -> usize {
    // TIOCGWINSZ 를 직접 쓰지 않는다 — 상수도 구조체도 플랫폼마다 달라서, 손으로
    // 적으면 한 OS 에서만 맞는 값이 박힌다. `COLUMNS` 도 못 믿는다: 셸이 export
    // 해야만 있고 파이프 너머로는 안 온다. 그래서 libc 에 맡기고, 그마저 실패하면
    // (파이프·리다이렉트) 80 으로 본다.
    #[cfg(unix)]
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 20
        {
            return ws.ws_col as usize;
        }
    }
    std::env::var("COLUMNS").ok().and_then(|s| s.parse().ok()).filter(|n| *n > 20).unwrap_or(80)
}

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

// ── statusline ──────────────────────────────────────────────────────────────
// collab-hooks/statusline.py 의 충실 이식. 출력 바이트 동형이 목표 — 세그먼트
// 순서·색·서식·마커를 py 와 같게 유지한다(골든 diff 로 검증). py 를 고치면
// 여기도 같이 고칠 것.

const SL_RESET: &str = "\x1b[0m";
const SL_BOLD: &str = "\x1b[1m";
const SL_DIM: &str = "\x1b[2m";

// 학생명 → accent hex (kasaterm theme.rs character_accent 와 동일 값)
const SL_STUDENT_HEX: &[(&str, &str)] = &[
    ("아로나", "4a90e2"),
    ("프라나", "e6e9f0"),
    ("미도리", "6bcf7f"),
    ("모모이", "ff6b6b"),
    ("유즈", "e64980"),
    ("아리스", "4c6ef5"),
    ("유우카", "7a5fd4"),
    ("시로코", "8fb8d8"),
    ("호시노", "f2a0c0"),
    ("코하루", "f27b9b"),
    ("히마리", "a88be0"),
    ("아루", "e85d4a"),
];
const SL_EFFORT_HEX: &[(&str, &str)] = &[
    ("low", "565f89"),
    ("medium", "7aa2f7"),
    ("high", "e0af68"),
    ("xhigh", "f7768e"),
    ("max", "bb9af7"),
];
const SL_C_MODEL: &str = "7aa2f7";
const SL_C_GIT: &str = "73daca";
const SL_C_DIR: &str = "bb9af7";
const SL_C_CTX: &str = "ff9e64";
const SL_C_SEP: &str = "565f89";
const SL_C_FALLBACK: &str = "a0a6b0";

// kasaterm pane 표식 — 옛 프사 자리표시자(5칸)를 프사 제거와 함께 1칸으로 줄인 것.
// **지우지 마라**: agents 뷰 판정·stale statusline 복구·standing 앵커가 이 문자의
// 존재를 근거로 삼는다(statusline.py 의 SPRITE 주석에 자세히 적어 뒀다).
const SL_SPRITE: &str = "\u{fffc}";

struct SlIcons {
    model: &'static str,
    git: &'static str,
    folder: &'static str,
    effort: &'static str,
}

fn sl_icons(set: &str) -> SlIcons {
    match set {
        "unicode" => SlIcons { model: ">", git: "⎇", folder: "▸", effort: "↯" },
        "plain" => SlIcons { model: "M", git: "git", folder: "dir", effort: "E" },
        _ => SlIcons {
            model: "\u{f233}",
            git: "\u{e0a0}",
            folder: "\u{f07b}",
            effort: "\u{f0e7}",
        },
    }
}

fn sl_env(name: &str) -> Option<String> {
    // py os.environ.get + truthiness — 빈 문자열은 미설정 취급.
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

fn sl_home() -> std::path::PathBuf {
    kasa_socket::home_dir().unwrap_or_default()
}

fn sl_read_json(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// 훅 stdin → (창 크기, 사용률%, 사용 토큰). 화면 표시와 GUI 보고가 같은 값을 쓰도록 한
/// 곳에서만 계산한다. 모델명으로 창을 추정하지 않는다 — 하네스가 준 값이 정본이다.
/// 예외는 Fable 5 뿐: 실제 1M 창인데 Claude Code(2.1.207)가 200k 로 잘못 보고해(#63015
/// 계열, 31만 토큰 요청이 실제 성공함을 확인) 알려진 진짜 창으로 재계산한다. 더 큰 쪽만
/// 취하는 보정이라 하네스 메타데이터가 고쳐지면 자동으로 무해해진다.
fn sl_context(d: &Value) -> (u64, f64, u64) {
    let ctx = d.get("context_window").cloned().unwrap_or(Value::Null);
    let mut pct = ctx.get("used_percentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let mut win = ctx.get("context_window_size").and_then(|v| v.as_u64()).unwrap_or(0);
    let tot = ctx.get("total_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let mid_owned = d
        .get("model")
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mid = mid_owned.split('[').next().unwrap_or("");
    let known: u64 = if mid == "claude-fable-5" { 1_000_000 } else { 0 };
    if known > win {
        win = known;
        pct = (tot as f64 / win as f64 * 100.0).min(100.0);
    }
    (win, pct, tot)
}

fn sl_git_branch(cwd: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn run_statusline() {
    use std::io::Read;
    let mut buf = String::new();
    let d: Value = match std::io::stdin().read_to_string(&mut buf) {
        Ok(_) => match serde_json::from_str(&buf) {
            Ok(v) => v,
            Err(_) => {
                println!("{} err{SL_RESET}", ansi_fg("f7768e"));
                return;
            }
        },
        Err(_) => {
            println!("{} err{SL_RESET}", ansi_fg("f7768e"));
            return;
        }
    };

    let cfg = sl_read_json(&sl_home().join(".claude/statusline-config.json")).unwrap_or(Value::Null);
    let ic = sl_icons(cfg.get("icon_set").and_then(|v| v.as_str()).unwrap_or("nerd-font"));
    let sep_char = cfg.get("separator").and_then(|v| v.as_str()).unwrap_or("┃");

    let cwd = d
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_default();
    let session_id = d.get("session_id").and_then(|v| v.as_str()).unwrap_or("");

    // claude 내부 cd 와 컨텍스트 창을 GUI 에 보고 — 자기 자신을 report-cwd 로 재실행
    // (비동기, statusline 출력을 지연시키지 않는다). pane 밖에선 무동작. 창을 함께
    // 보내는 이유는 transcript 의 model 에 `[1m]` 이 안 실려 GUI 가 1M 세션을 200k 로
    // 오판하기 때문 — 하네스가 준 이 값만이 정본이다.
    if let Some(pane) = sl_env("KASATERM_PANE_ID") {
        if !cwd.is_empty() {
            if let Ok(me) = std::env::current_exe() {
                let (ctx_win, _, ctx_tot) = sl_context(&d);
                let (win_s, tot_s) = (ctx_win.to_string(), ctx_tot.to_string());
                // 재시작 뒤 같은 모델·effort 로 되살리려고 함께 싣는다. `id` 는 **가공
                // 없이** — `[1m]` 을 떼면 되먹였을 때 1M 세션이 200k 로 강등된다.
                let model = d.pointer("/model/id").and_then(|v| v.as_str()).unwrap_or("");
                let effort = d.pointer("/effort/level").and_then(|v| v.as_str()).unwrap_or("");
                let _ = std::process::Command::new(me)
                    .args([
                        "report-cwd", &pane, &cwd, session_id, &win_s, &tot_s, model, effort,
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
    }

    // 세션 id 마커 — 렌더러가 conceal 지원을 공표(caps.json)한 경우에만 SGR8 로 싣는다.
    let mut sid_marker = String::new();
    if !session_id.is_empty() && sl_env("KASATERM_PANE_ID").is_some() {
        if let Some(caps) = sl_read_json(&sl_home().join(".config/kasaterm/caps.json")) {
            if caps.get("sgr_conceal").and_then(|v| v.as_bool()).unwrap_or(false) {
                let sid8: String = session_id.chars().take(8).collect();
                sid_marker = format!("\x1b[8m⟦{sid8}⟧\x1b[28m");
            }
        }
    }

    let sep = format!(" {SL_DIM}{}{sep_char}{SL_RESET} ", ansi_fg(SL_C_SEP));
    let mut parts: Vec<String> = Vec::new();

    let mut name = sl_env("KASATERM_CHARACTER");
    // 포크/attach 뷰(세션 id ≠ env anchor)만 세션→캐릭터 영속 바인딩을 정본으로.
    let forked_view = !session_id.is_empty()
        && std::env::var("KASATERM_SESSION_ID").ok().as_deref() != Some(session_id);
    if forked_view {
        if let Some(map) = sl_read_json(&sl_home().join(".config/kasaterm/session_characters.json"))
        {
            if let Some(bound) = map.get(session_id).and_then(|v| v.as_str()) {
                if !bound.is_empty() {
                    name = Some(bound.to_string());
                }
            }
        }
    }
    if let Some(ref name) = name {
        let hex = SL_STUDENT_HEX
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
            .unwrap_or(SL_C_FALLBACK);
        // pane 안에서는 학생을 안 쓴다 — pane 헤더가 이미 보여준다. 밖에서는 헤더가
        // 없으니 여기서만 알 수 있다. (statusline.py 와 출력 바이트가 같아야 한다.)
        if sl_env("KASATERM_PANE_ID").is_some() {
            parts.push(SL_SPRITE.to_string());
        } else {
            let c = ansi_fg(hex);
            parts.push(format!("{c}●{SL_RESET} {c}{SL_BOLD}{name}{SL_RESET}"));
        }
    }

    // ⑂ bg 배지 — anchor 불일치이되 사용자 주도 resume(shim 마커)은 제외.
    let user_resume = !session_id.is_empty()
        && (std::env::var("KASATERM_RESUMED_SID").ok().as_deref() == Some(session_id)
            || sl_env("KASATERM_RESUME_PICKER").is_some());
    if forked_view && !user_resume && sl_env("KASATERM_PANE_ID").is_some() {
        parts.push(format!("{SL_DIM}{}⑂ bg{SL_RESET}", ansi_fg(SL_C_FALLBACK)));
    }

    if let Some(model) = d
        .get("model")
        .and_then(|m| m.get("display_name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        // "(1M context)" 등 괄호 꼬리는 ctx% 의 "·1M" 과 중복 — 잘라 truncate 방지.
        let model = model.split(" (").next().unwrap_or(model);
        parts.push(format!("{}{SL_BOLD}{} {model}{SL_RESET}", ansi_fg(SL_C_MODEL), ic.model));
    }

    if let Some(branch) = sl_git_branch(&cwd).filter(|s| !s.is_empty()) {
        parts.push(format!("{}{} {branch}{SL_RESET}", ansi_fg(SL_C_GIT), ic.git));
    }

    let dir_name = std::path::Path::new(&cwd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    parts.push(format!("{}{} {dir_name}{SL_RESET}", ansi_fg(SL_C_DIR), ic.folder));

    let (win, pct, _) = sl_context(&d);
    let win_s = if win >= 1_000_000 {
        format!("·{}M", win / 1_000_000)
    } else if win > 0 {
        format!("·{}k", win / 1_000)
    } else {
        String::new()
    };
    let c_ctx = if pct >= 90.0 { ansi_fg("f7768e") } else { ansi_fg(SL_C_CTX) };
    parts.push(format!("{c_ctx}{pct:.0}%{SL_DIM}{win_s}{SL_RESET}"));

    if let Some(lvl) = d
        .get("effort")
        .and_then(|e| e.get("level"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        let hex = SL_EFFORT_HEX
            .iter()
            .find(|(k, _)| *k == lvl)
            .map(|(_, v)| *v)
            .unwrap_or("7aa2f7");
        parts.push(format!("{}{} {lvl}{SL_RESET}", ansi_fg(hex), ic.effort));
    }

    println!("{}{sid_marker}", parts.join(&sep));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 오른쪽 끝에 붙은 아주 좁은 pane 이 격자 밖을 짚지 않는다.
    ///
    /// 2026-08-28 에 `windows` 가 이 자리에서 통째로 죽었다(방 14개). 원인은
    /// `.min(W-1).max(x0+2)` 의 순서였고, `max` 가 상한을 덮어써 인덱스가 폭을
    /// 넘었다. 방이 많아 pane 이 잘게 쪼개질 때만 나오므로 손으로는 잘 안 걸린다.
    #[test]
    fn narrow_pane_at_the_edge_stays_inside_the_grid() {
        // 폭 0 짜리도 준다 — 저장된 비율이 반올림으로 그렇게 나올 수 있다.
        let rects = vec![
            ("%1".to_string(), 0u16, 0u16, 100u16, 100u16),
            ("%2".to_string(), 100, 100, 0, 0),
            ("%3".to_string(), 99, 99, 1, 1),
            ("%4".to_string(), 98, 0, 2, 100),
        ];
        let out = draw_boxes(&rects); // 패닉하면 여기서 끝난다
        assert!(!out.is_empty());
        assert!(out.lines().all(|l| l.chars().count() <= 46));
    }

    /// 방 하나를 잘게 쪼갠 실제 모양 — 회귀가 나면 여기서 먼저 걸린다.
    #[test]
    fn many_panes_in_one_window_do_not_panic() {
        let n = 19u16;
        let rects: Vec<_> = (0..n)
            .map(|i| {
                let w = 100 / n;
                (format!("%{i}"), i * w, 0, w, 100)
            })
            .collect();
        assert!(!draw_boxes(&rects).is_empty());
    }
}
