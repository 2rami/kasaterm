//! Thin CLI that wraps the cmux-compatible JSON-RPC protocol. Mirrors
//! the subset of cmux's official CLI commands that we use for testing,
//! scripting, and the eventual claude-code teammateMode handshake.
//!
//! Subcommands map 1:1 to protocol methods:
//!
//!   kasaterm-cli identify
//!   kasaterm-cli where [query]
//!   kasaterm-cli split  <left|right|up|down> [--focus]
//!   kasaterm-cli tell   <name|%N> <text>        # safe delivery to an agent
//!   kasaterm-cli tell --raw [%N] <text>         # write to a pane without the tell guard
//!   kasaterm-cli help                           # the full grouped list
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

use anyhow::{anyhow, Context, Result};
use crate::protocol::{Request, Response};
use crate::transport::LocalStream;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

struct ApiTarget { base: String, token_file: Option<std::path::PathBuf> }
static API_TARGET: std::sync::OnceLock<ApiTarget> = std::sync::OnceLock::new();

fn parse_api_target(args: &mut Vec<String>) -> Result<Option<ApiTarget>> {
    let mut base = None;
    let mut token_file = None;
    while args.first().is_some_and(|arg|matches!(arg.as_str(),"--api"|"--api-token-file")) {
        let option = args.remove(0);
        if args.is_empty() { return Err(anyhow!("{option} requires a value")); }
        let value = args.remove(0);
        if option == "--api" {
            if base.replace(value).is_some() { return Err(anyhow!("duplicate --api")); }
        } else { token_file = Some(std::path::PathBuf::from(value)); }
    }
    let Some(base) = base else {
        if token_file.is_some() { return Err(anyhow!("--api-token-file requires --api")); }
        return Ok(None);
    };
    if !(base.starts_with("http://") || base.starts_with("https://")) || base.chars().any(char::is_control) {
        return Err(anyhow!("--api requires an explicit HTTP or HTTPS base URL"));
    }
    Ok(Some(ApiTarget {base:base.trim_end_matches('/').to_owned(),token_file}))
}

pub fn main() {
    match run() {
        Ok(Some(resp)) => {
            // 사람이 터미널에서 직접 쳤으면(`to 맥미니` 같은 셰임) JSON 덩어리 대신
            // 문장 한 줄 — 실패는 그 이유, 성공은 요약/원격 id. 파이프·스크립트엔
            // 종전대로 wire 응답을 준다(`| jq .result`).
            use std::io::IsTerminal;
            if std::io::stdout().is_terminal() {
                if !resp.ok {
                    let why = resp.error.as_ref().map(|e| e.message.as_str()).unwrap_or("실패");
                    eprintln!("{why}");
                    std::process::exit(1);
                }
                let human = resp.result.as_ref().and_then(|r| {
                    r.get("summary")
                        .or_else(|| r.get("remote_id"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
                if let Some(line) = human {
                    println!("{line}");
                    std::process::exit(0);
                }
            }
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
    if let Some(target) = parse_api_target(&mut args)? { let _ = API_TARGET.set(target); }
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help" | "help") {
        print_help();
        return Ok(None);
    }
    let cmd = args.remove(0);
    // 살린 명령 안의 하위 동작은 플래그로 고른다 — 안쪽 이름(`tell:status` 따위)은 사람이 칠 명령이 아니다
    // (2026-09-29 CLI 정리: 명령 수를 줄이고 하던 일은 그대로).
    let cmd = match (cmd.as_str(), args.first().map(String::as_str)) {
        ("tell", Some("--status")) => { args.remove(0); "tell:status".to_string() }
        ("tell", Some("--raw")) => { args.remove(0); "tell:raw".to_string() }
        ("tell", Some("--key")) => { args.remove(0); "tell:key".to_string() }
        ("board", Some("--wait")) => { args.remove(0); "board:wait".to_string() }
        ("machines", Some("connect")) => { args.remove(0); "machines:connect".to_string() }
        ("machines", Some("move")) => { args.remove(0); "machines:move".to_string() }
        ("share", Some("open")) => { args.remove(0); "share:open".to_string() }
        ("tab", _) if args.iter().any(|a| a == "--server") => {
            args.retain(|a| a != "--server");
            "tab:server".to_string()
        }
        _ if cmd.contains(':') => return Err(anyhow!("unknown command: {cmd}")),
        _ => cmd,
    };
    if API_TARGET.get().is_some() {
        if !matches!(cmd.as_str(),"board"|"board-watch"|"activity"|"tell"|"tell:status") {
            return Err(anyhow!("--api supports board, board-watch, activity and tell"));
        }
        if matches!(cmd.as_str(),"board"|"board-watch") && !args.iter().any(|s|matches!(s.as_str(),"--all"|"--local")) {
            args.push("--all".into());
        }
    }
    // `board-watch` is a polling loop, not a single round-trip: it streams one
    // line per *changed* pane so a Claude Code Monitor can watch the board and
    // wake on transitions (a worker going `waiting` for a permission prompt,
    // finishing → `idle`, etc.) without dumping the whole board every tick.
    if cmd == "board-watch" {
        if args.iter().any(|s|matches!(s.as_str(),"--all"|"--local"|"--since"|"--json")) {
            let socket_path = resolve_socket_path()?;
            run_collab_watch(&socket_path,&args)?;
            return Ok(None);
        }
        let interval = args
            .first()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(3);
        let socket_path = resolve_socket_path()?;
        run_board_watch(&socket_path, interval)?;
        return Ok(None);
    }
    // `share` — KASA-share 결과물 폴더. 앱을 안 거친다: 폴더를 만들고 상태 파일을 읽을
    // 뿐이라 앱이 꺼져 있어도 되고, 옮기는 일은 앱이 다음 훑기에 한다.
    if cmd == "share" {
        return run_share(&args);
    }
    // `op` — 1Password 비밀을 폰 Face ID 승인 한 번으로(docs/op-faceid-approval.md). 값은 이 프로세스에만 온다.
    if cmd == "op" {
        return run_op(&args);
    }
    // `app-restart` — 등록된 기기의 카사텀 앱 재시작 계획·상태. 실행은 사람 승인 흐름이 생기기 전까지 거부한다.
    #[cfg(feature = "app-update")]
    if cmd == "app-restart" {
        return run_app_restart(&args);
    }
    // `app-update` — 기기의 앱 업데이트 작업 걸기·상태. 승인은 조종 기기가 오케스트레이터에서 이미 소비한 것만, 기기 앱이 다시 판정한다.
    #[cfg(feature = "app-update")]
    if cmd == "app-update" {
        return run_app_update(&args);
    }
    // 클립보드 — 값이 대화·인자·기록에 찍히지 않게 하는 문 셋(2026-09-10 지시 「env 키
    // 같은 것도 클립보드에 있어 하면 안전하게」). usemap CLI 의 `--clipboard` 와 같은 생각.
    if cmd == "copy" && args.first().is_some_and(|a| a == "--secret") {
        let socket_path = resolve_socket_path()?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).context("표준입력 읽기")?;
        let text = text.trim_end_matches(['\n', '\r']).to_string();
        if text.trim().is_empty() {
            return Err(anyhow!("copy --secret 은 값을 표준입력으로 받는다 — `printf '%s' \"$KEY\" | kasaterm-cli copy --secret`"));
        }
        let req = Request {
            id: json!("copy-secret"),
            method: "clipboard.set".into(),
            params: json!({ "text": text, "secret": true }),
        };
        let resp = roundtrip(&socket_path, &req)?;
        if !resp.ok {
            return Err(anyhow!("{}", resp.error.as_ref().map(|e| e.message.as_str()).unwrap_or("복사 실패")));
        }
        println!("비밀값 복사됨 · {}자 — 목록·폰엔 가려 보인다", text.chars().count());
        return Ok(None);
    }
    if cmd == "paste" && !args.is_empty() {
        let socket_path = resolve_socket_path()?;
        let get = Request { id: json!("paste"), method: "clipboard.get".into(), params: json!({}) };
        let resp = roundtrip(&socket_path, &get)?;
        if !resp.ok {
            return Err(anyhow!("{}", resp.error.as_ref().map(|e| e.message.as_str()).unwrap_or("클립보드 읽기 실패")));
        }
        let result = resp.result.clone().unwrap_or(Value::Null);
        let text = result.get("text").and_then(Value::as_str).unwrap_or("").to_string();
        let secret = result.get("secret").and_then(Value::as_bool).unwrap_or(false);
        if text.is_empty() {
            return Err(anyhow!("클립보드가 비었다"));
        }
        match args[0].as_str() {
            "--show" => {
                println!("{text}");
                return Ok(None);
            }
            "--into" => {
                // 값은 이 프로세스에서 pane 으로 곧장 간다 — 부른 쪽(캐릭터) 화면에는 글자
                // 수만 남는다. 괄호붙임(bracketed paste)으로 감싸 한 덩어리로 들어가고,
                // Enter 는 안 친다 — 붙인 뒤 사람이 확인하고 넘기는 자리다.
                let surface = args
                    .get(1)
                    .cloned()
                    .or_else(|| std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty()))
                    .ok_or_else(|| anyhow!("paste --into 뒤에 pane 을 주거나 $KASATERM_PANE_ID 가 있어야 한다"))?;
                // 감싸개는 여기서 안 두른다 — 그 pane 이 bracketed paste 를 켰는지는
                // 서버만 안다. 여기서 두르면 안 켠 앱에 `[200~` 글자가 튀어나온다.
                let send = Request {
                    id: json!("paste-into"),
                    method: "surface.paste".into(),
                    params: json!({ "surface_id": surface, "text": text }),
                };
                let r = roundtrip(&socket_path, &send)?;
                if !r.ok {
                    return Err(anyhow!("{}", r.error.as_ref().map(|e| e.message.as_str()).unwrap_or("붙여넣기 실패")));
                }
                println!("{surface} 에 붙여넣음 · {}자{}", text.chars().count(), if secret { " (비밀값)" } else { "" });
                return Ok(None);
            }
            "--env" => {
                let var = args.get(1).filter(|v| !v.is_empty() && *v != "--").cloned()
                    .ok_or_else(|| anyhow!("paste --env <VAR> -- <명령…>"))?;
                let dash = args.iter().position(|a| a == "--").ok_or_else(|| anyhow!("paste --env <VAR> -- <명령…> — `--` 뒤에 돌릴 명령"))?;
                let command = &args[dash + 1..];
                let Some((prog, rest)) = command.split_first() else {
                    return Err(anyhow!("`--` 뒤에 돌릴 명령이 없다"));
                };
                let status = std::process::Command::new(prog)
                    .args(rest)
                    .env(&var, &text)
                    .status()
                    .with_context(|| format!("{prog} 실행"))?;
                std::process::exit(status.code().unwrap_or(1));
            }
            other => return Err(anyhow!("paste 의 모르는 옵션 {other} — --into · --env · --show")),
        }
    }
    if cmd == "paste" {
        // 값을 찍기 전에 비밀인지 본다 — 찍힌 값은 대화에서 못 지운다.
        let socket_path = resolve_socket_path()?;
        let get = Request { id: json!("paste"), method: "clipboard.get".into(), params: json!({}) };
        let resp = roundtrip(&socket_path, &get)?;
        if resp.ok && resp.result.as_ref().and_then(|r| r.get("secret")).and_then(Value::as_bool).unwrap_or(false) {
            let chars = resp.result.as_ref().and_then(|r| r.get("chars")).and_then(Value::as_u64).unwrap_or(0);
            println!("클립보드에 비밀값이 있다({chars}자) — 값을 안 찍는다. pane 에 붙이려면 `paste --into`, 명령에 주려면 `paste --env VAR -- <명령>`, 정말 봐야 하면 `paste --show`.");
            return Ok(None);
        }
        return Ok(Some(resp));
    }
    // `split --count N` 은 **한 번의 호출로** pane N 개를 배치한다.
    //
    // 예전엔 여기서 split 을 N 번 부르면서 2회차부터 직전에 만든 pane 을 대상으로
    // 삼았다. ⌘D 를 연달아 누른 것과 같은 모양이라 몫이 1/2 → 1/4 → 1/8 로
    // 반감하고, 넷을 부르면 마지막 학생이 화면의 1/16 이었다 — 사용자가 매번 드래그로
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
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                });
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
                    Some(
                        r.error
                            .map(|e| e.message)
                            .unwrap_or_else(|| "사유 없음".into()),
                    ),
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
    // 학생 한 명을 서브에이전트처럼 부르고 기다리는 두 명령. 서브에이전트는 보드·화면에 안 보여
    // 사람이 진행을 못 지켜본다 — 그런데 학생 소환은 쪼개기·부팅·보드 확인·tell 네 단계라 Claude 가
    // 한 번에 끝나는 Agent 도구로 흘렀다(2026-09-28). `summon` 이 그 넷을, `board --wait` 가 완료 기다리기를 맡는다.
    if cmd == "summon" {
        let socket_path = resolve_socket_path()?;
        run_summon(&socket_path, &args)?;
        return Ok(None);
    }
    if cmd == "board:wait" {
        let socket_path = resolve_socket_path()?;
        std::process::exit(run_wait(&socket_path, &args)?);
    }
    // `close <id>…` — pane 을 한 번에 닫는다. 일 끝난 학생은 인사말·완료 보고를 주고받는 대신
    // 그냥 닫는다(회수할 게 있으면 git 이 막고, --force 로만 넘는다).
    if cmd == "close" {
        let socket_path = resolve_socket_path()?;
        run_dismiss(&socket_path, &args)?;
        return Ok(None);
    }
    // `machines` — 명부 기계 목록을 사람 눈에 맞춰 찍는다(`to` 셰임의 `ls`). 이 pane
    // 이 어느 기계의 거울이면 그 줄에 `*`, 아니면 「이 기계」 줄에 `*`.
    if cmd == "machines" {
        let socket_path = resolve_socket_path()?;
        let from = std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty());
        let resp = roundtrip(
            &socket_path,
            &Request {
                id: "machines".into(),
                method: "machine.list".into(),
                params: json!({ "from": from }),
            },
        )?;
        if !resp.ok {
            anyhow::bail!(
                "{}",
                resp.error.map(|e| e.message).unwrap_or_else(|| "machine.list 실패".into())
            );
        }
        let r = resp.result.unwrap_or(Value::Null);
        let here = r.get("here").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let rows = r.get("machines").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        // `--names` — 라벨만 한 줄씩. `to` 의 zsh 탭 완성이 후보로 읽는다.
        if std::env::args().any(|a| a == "--names") {
            for m in &rows {
                if let Some(l) = m.get("label").and_then(|v| v.as_str()) {
                    println!("{l}");
                }
            }
            return Ok(None);
        }
        if rows.is_empty() {
            println!("등록된 기계가 없어요 — ~/.config/kasaterm/machines.json 에 적으면 여기 떠요");
            return Ok(None);
        }
        println!("{} 이 기계", if here.is_empty() { "*" } else { " " });
        for m in rows {
            let label = m.get("label").and_then(|v| v.as_str()).unwrap_or("?");
            let online = m.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
            let n = |k: &str| m.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
            let state = if !online {
                match m.get("ago_secs").and_then(|v| v.as_u64()) {
                    None => "안 닿음 — 한 번도 못 닿았어요".to_string(),
                    Some(s) if s < 60 => format!("안 닿음 — {s}초 전까지"),
                    Some(s) if s < 3600 => format!("안 닿음 — {}분 전까지", s / 60),
                    Some(s) => format!("안 닿음 — {}시간 전까지", s / 3600),
                }
            } else {
                let mut s = if n("students") == 0 {
                    "캐릭터 없음".to_string()
                } else {
                    format!("캐릭터 {}", n("students"))
                };
                if n("waiting") > 0 {
                    s.push_str(&format!(" · 기다림 {}", n("waiting")));
                }
                if n("mirrored") > 0 {
                    s.push_str(&format!(" · 거울 {}", n("mirrored")));
                }
                if m.get("guest").and_then(|v| v.as_bool()).unwrap_or(false) {
                    s.push_str(" · 그쪽이 열어 둔 길");
                }
                if !m.get("build_match").and_then(|v| v.as_bool()).unwrap_or(true) {
                    s.push_str(" · 빌드 다름");
                }
                s
            };
            let mark = if label == here { "*" } else { " " };
            let ssh = m
                .get("ssh")
                .and_then(|v| v.as_str())
                .map(|t| format!("   ssh {t}"))
                .unwrap_or_default();
            println!("{mark} {label:<12} {state}{ssh}");
        }
        return Ok(None);
    }
    // `closed [%pane]` — 되살리기 목록. pane 을 주면 그 항목을 **진짜 끈다**.
    //
    // 닫은 pane 은 죽지 않는다 — 프로세스를 물고 이 목록에 앉아 있다가 10개를 넘겨
    // 밀려날 때 죽는다. 그래서 `dismiss` 로 정리한 학생 claude 들이 계속 살아 있는데,
    // 그 사실이 GUI 밖에서는 보이지도 않았다(사용자 2026-08-06).
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
    // `sessions` — 터미널 안 세션 목록. claude 자체 /resume 은 teamName 이
    // 기록된 세션(=팀 트리플로 뜨는 kasaterm pane 세션 전부)을 무조건 숨기므로,
    // jsonl 직스캔으로 팀 세션까지 전부 보여주고 캐릭터색·캐릭터명으로 구분한다.
    // 디스크만 읽어 GUI 가 죽어 있어도 동작.
    if cmd == "sessions" {
        run_sessions_picker(&args)?;
        return Ok(None);
    }
    // `statusline` — pane claude 의 statusLine 커맨드(stdin JSON → 한 줄 출력).
    if cmd == "statusline" {
        run_statusline();
        return Ok(None);
    }
    // `claude-trust <폴더>` — pane claude shim 이 실행 직전에 부른다(폴더 신뢰 화면 선해결).
    // 소켓을 안 거쳐 앱이 없어도 돈다. 실패는 조용히 — 화면은 앱이 한 번 넘겨 준다.
    if cmd == "claude-trust" {
        if let Some(dir) = args.first() {
            crate::claude_trust::preseed(std::path::Path::new(dir));
        }
        return Ok(None);
    }
    let mut args = args;
    // 사람은 주소 JSON 을 안 친다(2026-09-16 지시 「몇 개 안 쳐도 바로 되게」) —
    // `tell 이름 본문` 은 보드에서 주소를 찾고, `tell --status ID` 는 보낼 때 적어 둔 주소를 쓴다.
    let tell_label = if cmd == "tell" { resolve_tell_target(&mut args)? } else { None };
    if cmd == "tell:status" && args.len() == 1 && !args[0].starts_with("--") {
        let address = load_receipt(&args[0]).ok_or_else(|| anyhow!(
            "이 ID 의 주소를 모르겠어요 — 이 기계에서 보낸 것이 아니면 --address 를 함께 주세요"
        ))?;
        args.push("--address".into());
        args.push(address.to_string());
    }
    // 오케스트레이터가 띄운 창(KASATERM_ORIGIN)이면 done 이 그쪽 보고함에도 넣는다 — 보고 명령이 둘이면
    // 한쪽만 하고 끝내는 일이 생긴다. 보고가 거부되면(비밀처럼 보이는 글·다음 할 일 빠짐) 판 완료도 안 적는다.
    if cmd == "done" {
        let (board_args, report) = split_done_args(&args)?;
        if crate::nacho_inbox::origin_from_env().is_some() {
            if let Some(failed) = run_orchestrator_report(&report)? {
                return Ok(Some(failed));
            }
        }
        args = board_args;
    }
    let mut request = build_request(&cmd, &args)?;
    // 줄 선 쪽지가 버려지면 보낸 창에 토스트로 알려 달라고 이 기기 앱에 맡긴다. 다른 기기로 곧장 보내는
    // `--api` 는 그 기기가 이 창을 모르니 빼고, 창 밖(사람이 친 셸)도 알릴 데가 없어 뺀다.
    if cmd == "tell" && API_TARGET.get().is_none() {
        if let Some(pane) = std::env::var("KASATERM_PANE_ID").ok().filter(|p| !p.is_empty()) {
            let label = tell_label.clone().or_else(|| request.params["surface_id"].as_str().map(str::to_owned))
                .or_else(|| request.params.pointer("/address/surface_id").and_then(Value::as_str).map(str::to_owned));
            request.params["notify"] = json!({ "surface": pane, "label": label });
        }
    }
    let socket_path = resolve_socket_path()?;
    let mut response = roundtrip(&socket_path, &request)?;
    // 오케스트레이터가 띄운 세션이면 세션 id(기록 파일 이름)에 표식을 남긴다 — 앱 재시작 복원이
    // `--resume` 할 때 되붙인다(`nacho_inbox::remember_origin`). 실패해도 bind 는 성공이다.
    if cmd == "bind-transcript" && response.ok {
        if let (Some(origin), Some(sid)) = (
            crate::nacho_inbox::origin_from_env(),
            args.first().and_then(|p| Path::new(p).file_stem()).and_then(|s| s.to_str()),
        ) {
            let _ = crate::nacho_inbox::remember_origin(sid, &origin);
        }
    }
    if cmd == "tell" && response.ok {
        if let (Some(id), Some(address)) = (
            request.params.get("message_id").and_then(|v| v.as_str()),
            response.result.as_ref().and_then(|r| r.get("address")).filter(|a| a.is_object()),
        ) {
            let (id, address) = (id.to_string(), address.clone());
            save_receipt(&id, &address);
            // 보관(`accepted`)만 보고 나가면 「갔나?」를 확인할 길이 `tell --status` 나 peek 뿐이다.
            // 붙여넣기와 Enter 는 1초 안에 끝나므로 여기서 그 결과까지 보고 나간다
            // (2026-09-21 지시 「전송됐는지 peek 말고 빠르게」).
            if let Some(settled) = await_tell_settled(&socket_path, &id, &address) {
                eprintln!("tell: {}", tell_state_line(&settled));
                response.result = Some(settled);
            }
        }
    }
    if cmd == "tab:server" {
        if let Some(error) = response.error.as_mut() {
            if error.code == crate::protocol::codes::METHOD_NOT_FOUND {
                error.message = "이 앱은 서버 복원 등록을 지원하지 않아요. 앱을 업데이트한 뒤 다시 등록해주세요. 서버는 실행하지 않았어요.".into();
            }
        }
    }
    // 칸이 열리고 닫힐 때마다 격자가 다시 짜이고 웹·문서는 연 칸의 탭으로 열린다 — 번호만 들고는
    // 제 창도 남의 창도 못 찾으니, 방·행·열·탭으로 말해 준다. `--json` 이면 그대로.
    if cmd == "where" && response.ok && !args.iter().any(|a| a == "--json") {
        let query: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
        let me = std::env::var("KASATERM_PANE_ID").ok();
        println!("{}", render_where(&response, &query.join(" "), me.as_deref()));
        return Ok(None);
    }
    // 턴 시작 훅이 부른다 — 이 pane 에 tell 로 「지금 일」이 들어왔으면 claude 의 공식 창구(UserPromptSubmit
    // 훅 출력의 `sessionTitle`)로 세션 이름을 맞춘다. 그 밖엔 아무것도 안 낸다: 훅 stdout 은 claude 가 읽는다.
    if cmd == "turn" {
        if let Some(title) = response.result.as_ref().and_then(|r| r.get("session_title")).and_then(Value::as_str) {
            println!("{}", json!({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "sessionTitle": title}}));
        }
        return if response.ok { Ok(None) } else { Ok(Some(response)) };
    }
    // `activity` 는 사람(과 학생)이 읽는 자리다 — board 처럼 기계가 파싱하는 게
    // 아니라 「쟤 왜 저러나」를 눈으로 훑는 용도라, JSON 대신 시간순 목록으로 낸다.
    if cmd == "activity" && response.ok {
        println!("{}", render_activity(&response));
        return Ok(None);
    }
    Ok(Some(response))
}

/// 붙여넣기와 Enter 가 끝날 때까지만 기다린다. 상대가 일하는 중이면 큐에 남아 보관
/// 상태로 오래 있을 수 있으니, 그때는 기다리지 않고 지금 상태를 그대로 돌려준다 —
/// 기다리는 시간이 길어지면 「빠르게 확인」이라는 목적 자체가 사라진다.
fn await_tell_settled(socket_path: &str, id: &str, address: &Value) -> Option<Value> {
    // 첫 쓰기는 160ms 뒤 확인으로 이어진다. 그 두 배를 상한으로 잡고 짧게 되묻는다.
    const TRIES: usize = 12;
    const GAP: std::time::Duration = std::time::Duration::from_millis(160);
    let mut last = None;
    for attempt in 0..TRIES {
        std::thread::sleep(GAP);
        let request = Request {
            id: json!("tell-settle"),
            method: "collab.tell_status".into(),
            params: json!({ "message_id": id, "address": address }),
        };
        let Ok(response) = roundtrip(socket_path, &request) else { break };
        if !response.ok {
            break;
        }
        let Some(receipt) = response.result else { break };
        let settled = receipt
            .get("state")
            .and_then(|s| s.as_str())
            // 보관·전달중은 아직 가는 길이다. 그 밖은 이미 결론이라 더 기다릴 것이 없다.
            .is_some_and(|state| !matches!(state, "accepted" | "dispatching"));
        last = Some(receipt);
        if settled {
            break;
        }
        let _ = attempt;
    }
    last
}

/// 사람이 한눈에 읽을 한 줄. 영수증 JSON 은 그대로 표준 출력으로도 나간다.
fn tell_state_line(receipt: &Value) -> String {
    let state = receipt.get("state").and_then(|s| s.as_str()).unwrap_or("?");
    let reason = receipt.get("reason").and_then(|s| s.as_str()).unwrap_or_default();
    if let (true, Some(hold)) = (state == "accepted", crate::tell::Hold::from_reason(reason)) {
        let until = receipt.get("expires_at_ms").and_then(Value::as_u64).and_then(crate::tell::clock_hm)
            .unwrap_or_else(|| "만료 시각".into());
        return format!("대기 — {}. {} ({until}까지 못 들어가면 버려진다. 버려지면 이 창 위 토스트로만 알린다)", hold.cause(), hold.remedy());
    }
    let said = match state {
        "submitted" => "들어갔다(모델이 읽었는지는 별개)",
        "accepted" => "아직 큐 — 상대 입력창이 비면 들어간다",
        "dispatching" => "보내는 중",
        "failed" => "실패",
        "uncertain" => "확인 못 함 — 같은 ID 로만 다시 확인해라",
        _ => state,
    };
    if reason.is_empty() { said.to_string() } else { format!("{said} · {reason}") }
}


/// Poll `collab.board` AND this pane's inbox every `interval_secs`, printing
/// one Monitor event line per change: a pane whose status/intent changed (or
/// `closed`), and a new `✉` message addressed to `$KASATERM_PANE_ID`. The first
/// tick baselines both (board state + existing messages) silently so a fresh
/// watch doesn't replay history. Transient failures are swallowed. Never
/// returns (Ctrl-C / Monitor timeout ends it).
fn run_collab_watch(socket_path: &str, args: &[String]) -> Result<()> {
    let mut scope = "all";
    let mut since: Option<String> = None;
    // 변경분 조회(`collab.changes`)는 캐시 한 번이라 1초 폴링이 싸다 — 3초는 판이 신호로
    // 바로 갱신되는 지금 병목이었다.
    let mut interval = 1;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all" => scope = "all",
            "--local" => scope = "local",
            "--json" => (),
            "--since" => since = Some(args.next().ok_or_else(||anyhow!("--since requires a cursor"))?.clone()),
            text => interval = text.parse::<u64>().map_err(|_|anyhow!("unknown board-watch argument"))?.clamp(1,60),
        }
    }
    let request = |method: &str,params: Value| -> Result<Value> {
        let response = roundtrip(socket_path,&Request{id:"collab-watch".into(),method:method.into(),params})?;
        if !response.ok { return Err(anyhow!(response.error.map(|e|e.message).unwrap_or_else(||"collaboration request failed".into()))); }
        response.result.ok_or_else(||anyhow!("collaboration response missing result"))
    };
    if since.is_none() {
        let snapshot = request("collab.snapshot",json!({"scope":scope}))?;
        since = snapshot["cursor"].as_str().map(str::to_owned);
        println!("{}",json!({"kind":"snapshot","snapshot":snapshot,"cursor":since}));
    }
    loop {
        let batch = request("collab.changes",json!({"scope":scope,"since":since,"limit":100}))?;
        if batch["reset_required"] == true {
            println!("{batch}");
            return Err(anyhow!("collaboration cursor needs a fresh snapshot"));
        }
        let next = batch["cursor"].as_str().ok_or_else(||anyhow!("collaboration cursor missing"))?.to_owned();
        if since.as_deref() != Some(&next) { println!("{batch}"); }
        since = Some(next);
        std::io::stdout().flush()?;
        if batch["has_more"] != true { std::thread::sleep(std::time::Duration::from_secs(interval)); }
    }
}

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
                    let mut status = e
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
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
                            let sum = e.get("done_summary").and_then(|v| v.as_str()).unwrap_or("");
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
            eprintln!(
                "[bw-dbg] me={me:?} path={msgs_path:?} exists={} seen={}",
                msgs_path.exists(),
                seen_msgs.len()
            );
        }
        if !me.is_empty() {
            if let Ok(content) = std::fs::read_to_string(&msgs_path) {
                let mut out = std::io::stdout().lock();
                for line in content.lines() {
                    let Ok(m) = serde_json::from_str::<Value>(line) else {
                        continue;
                    };
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

/// 학생 소환 브리프 끝에 붙이는 한 줄 — 완료 보고가 없으면 `wait` 가 끝나지 않는다.
const SUMMON_DONE_HINT: &str = "끝나면 `kasaterm-cli done succeeded '한 줄 요약'`(못 끝냈으면 failed)으로 보고해 주세요.";
/// 부팅한 claude 가 보드에 설 때까지 기다리는 상한.
const SUMMON_BOOT_SECS: u64 = 90;

/// 부팅 명령이 새 codex 대화를 여는가(`codex`·`시로코 codex`·`codex --model x`). 이어 열기·한 번 실행 같은
/// 서브커맨드는 첫 입력 인자를 받는 자리가 아니라 빼고, 따옴표로 지시를 이미 준 명령에는 덧붙이지 않는다.
fn boots_fresh_codex(boot: &str) -> bool {
    let words: Vec<&str> = boot.split_whitespace().collect();
    let Some(at) = words.iter().position(|w| *w == "codex") else { return false };
    !words[at + 1..].iter().any(|w| w.starts_with(['\'', '"'])
        || matches!(*w, "resume" | "fork" | "exec" | "e" | "review" | "login" | "logout" | "mcp" | "apply" | "a" | "cloud" | "help"))
}

/// 갓 부팅한 학생에게 보낸 tell 의 거절 중 기다리면 풀리는 것 — claude 판정이나 대화 번호가 아직 안 섰다.
fn tell_target_booting(why: &str) -> bool {
    ["shell or unsupported harness", "bound conversation differs", "tell withheld"]
        .iter()
        .any(|reason| why.contains(reason))
}

/// `summon [--cwd 폴더] [--cmd 부팅명령] [--name 제목] [--tab] [--stdin] [브리프…]` — 부른 pane 옆에 학생을
/// 세우고, 그 claude 가 보드에 설 때까지 기다렸다가 브리프를 tell 로 건넨다. 학생의 `done` 은 부른 pane
/// 입력창으로 들어온다(`spawned_by`). 창은 실패해도 닫지 않는다 — 사람이 무엇이 멎었는지 봐야 한다.
fn run_summon(socket_path: &str, args: &[String]) -> Result<()> {
    let (mut cwd, mut name, mut boot) = (None::<String>, None::<String>, "claude".to_string());
    let (mut tab, mut stdin) = (false, false);
    let mut words: Vec<String> = Vec::new();
    let value = |i: usize| args.get(i + 1).cloned().ok_or_else(|| anyhow!("{} 뒤에 값이 필요해요", args[i]));
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--cwd" => { cwd = Some(value(i)?); i += 2; }
            "--cmd" => { boot = value(i)?; i += 2; }
            "--name" => { name = Some(value(i)?); i += 2; }
            "--tab" => { tab = true; i += 1; }
            "--stdin" => { stdin = true; i += 1; }
            "--" => { words.extend(args[i + 1..].iter().cloned()); break; }
            a if a.starts_with("--") => return Err(anyhow!("모르는 옵션: {a}")),
            _ => { words.push(arg.clone()); i += 1; }
        }
    }
    let brief = if stdin {
        if !words.is_empty() { return Err(anyhow!("--stdin 과 브리프 인자는 함께 못 써요")); }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        text
    } else {
        words.join(" ")
    };
    if brief.trim().is_empty() {
        return Err(anyhow!("summon 은 브리프가 필요해요 — kasaterm-cli summon --cwd <레포> \"할 일\""));
    }
    let cwd = match cwd { Some(dir) => std::path::PathBuf::from(dir), None => std::env::current_dir()? };
    let cwd = cwd.canonicalize().with_context(|| format!("{} 폴더가 없어요", cwd.display()))?;

    let from = std::env::var("KASATERM_PANE_ID").ok().filter(|s| !s.is_empty());
    let since = epoch_ms();
    let (method, params) = if tab {
        ("surface.new_tab", json!({ "outer": from, "focus": false }))
    } else {
        ("surface.split", json!({ "direction": "auto", "focus": false, "from": from }))
    };
    let made = roundtrip(socket_path, &Request { id: json!("summon"), method: method.into(), params })?;
    if !made.ok {
        return Err(anyhow!(made.error.map(|e| e.message).unwrap_or_else(|| "pane 을 못 만들었어요".into())));
    }
    let surface = made.result.as_ref().and_then(|r| r.pointer("/surface/id")).and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("새 pane 번호를 못 받았어요"))?.to_string();
    if let Some(title) = &name {
        let _ = roundtrip(socket_path, &Request { id: json!("summon"), method: "surface.rename".into(),
            params: json!({ "surface_id": surface, "title": title }) });
    }

    let mut body = brief.trim().to_string();
    if !body.contains("kasaterm-cli done") {
        body.push_str("\n\n");
        body.push_str(SUMMON_DONE_HINT);
    }
    let title = name.clone().or_else(|| brief_title(&brief)).map(|t| crate::tell::normalize_title(&t)).transpose()?
        .filter(|t| !t.is_empty());
    let body = mark_tell_sender(body, std::env::var("KASATERM_CHARACTER").ok().as_deref());
    let body = crate::tell::normalize(&body)?;

    // codex 는 첫 입력 전엔 대화 기록이 없어 tell 이 겨눌 주소(대화 번호)가 영영 안 선다 — 브리프를 첫
    // 입력 인자로 넘긴다(2026-10-02 실측: `--cmd '호시노 codex'` 가 90초 뒤 「안 떴어요」로 끝났다).
    let first_prompt = boots_fresh_codex(&boot);
    // 갓 만든 pane 은 번호 재사용 때문에 「pane 없음」 가드가 한두 번 헛걸린다(실측) — 잠깐 두고 다시 보낸다.
    let line = if first_prompt {
        format!("cd {} && {boot} {}\n", shell_quote(&cwd.to_string_lossy()), shell_quote(&body))
    } else {
        format!("cd {} && {boot}\n", shell_quote(&cwd.to_string_lossy()))
    };
    let mut refused = None;
    for attempt in 0..3 {
        if attempt > 0 { std::thread::sleep(std::time::Duration::from_millis(1500)); }
        let sent = roundtrip(socket_path, &Request { id: json!("summon"), method: "surface.send_text".into(),
            params: json!({ "surface_id": surface, "text": line }) })?;
        refused = (!sent.ok).then(|| sent.error.map(|e| e.message).unwrap_or_default());
        if refused.is_none() { break; }
    }
    if let Some(why) = refused {
        return Err(anyhow!("{surface} 에 부팅 명령을 못 넣었어요: {why}"));
    }
    let wait = format!("kasaterm-cli board --wait {surface} --since {since}");
    if first_prompt {
        let started = std::time::Instant::now();
        let row = loop {
            let row = snapshot_rows(socket_path, true).ok().and_then(|rows| rows.into_iter()
                .find(|p| row_address(p, "surface_id") == surface && !row_address(p, "session_id").is_empty()));
            if let Some(row) = row { break row; }
            if started.elapsed() > std::time::Duration::from_secs(SUMMON_BOOT_SECS) {
                return Err(anyhow!("{surface} 에 {SUMMON_BOOT_SECS}초 안에 학생이 안 떴어요 — `kasaterm-cli peek {surface}` 로 화면을 보세요(창은 그대로 뒀어요)"));
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        };
        if let (Some(title), None) = (&title, &name) {
            let _ = roundtrip(socket_path, &Request { id: json!("summon"), method: "surface.rename".into(),
                params: json!({ "surface_id": surface, "title": title }) });
        }
        let character = row_text(&row, "character");
        let who = if character.is_empty() { surface.clone() } else { format!("{character}({surface})") };
        println!("{who} 소환 · 지시는 첫 입력으로 넣음");
        println!("{}", if from.is_some() { format!("done 보고는 이 창 입력으로 들어와요. 막고 기다리려면: {wait}") } else { format!("기다리려면: {wait}") });
        return Ok(());
    }

    // 브리프는 claude 가 뜬 뒤에만 tell 로 — 셸 명령줄에 섞이면 부팅이 깨진다(skills/kasapane).
    // 보드에 대화 번호가 서는 것과 tell 이 받는 조건(claude 판정·살아 있는 대화와 번호 일치)은 1~2초
    // 어긋나 선다. 그 틈에 보낸 첫 tell 이 두 번 연속 거절됐다(2026-09-29). 거절은 접수 전이라 보드를
    // 다시 읽어 새 주소·새 ID 로 보낸다.
    let started = std::time::Instant::now();
    let (row, message_id, address, told) = loop {
        let row = snapshot_rows(socket_path, true).ok().and_then(|rows| rows.into_iter()
            .find(|p| row_address(p, "surface_id") == surface && !row_address(p, "session_id").is_empty()));
        let timed_out = started.elapsed() > std::time::Duration::from_secs(SUMMON_BOOT_SECS);
        if let Some(row) = row {
            let message_id = crate::tell::new_message_id();
            let address = row.get("address").cloned().unwrap_or(Value::Null);
            let mut params = json!({ "message_id": message_id, "address": address, "body": body });
            if let Some(title) = &title { params["title"] = json!(title); }
            if let Some(pane) = &from { params["notify"] = json!({ "surface": pane, "label": surface }); }
            let told = roundtrip(socket_path, &Request { id: json!("summon"), method: "collab.tell".into(), params })?;
            if told.ok {
                break (row, message_id, address, told);
            }
            let why = told.error.map(|e| e.message).unwrap_or_default();
            if timed_out || !tell_target_booting(&why) {
                return Err(anyhow!("{surface} 에 학생은 떴는데 지시를 못 넣었어요: {why}"));
            }
        } else if timed_out {
            return Err(anyhow!("{surface} 에 {SUMMON_BOOT_SECS}초 안에 학생이 안 떴어요 — `kasaterm-cli peek {surface}` 로 화면을 보세요(창은 그대로 뒀어요)"));
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    let address = told.result.as_ref().and_then(|r| r.get("address")).filter(|a| a.is_object()).cloned().unwrap_or(address);
    save_receipt(&message_id, &address);
    let state = await_tell_settled(socket_path, &message_id, &address)
        .map(|receipt| tell_state_line(&receipt)).unwrap_or_else(|| "접수".into());
    let character = row_text(&row, "character");
    let who = if character.is_empty() { surface.clone() } else { format!("{character}({surface})") };
    println!("{who} 소환 · 지시 {state} · 영수증 {message_id}");
    if from.is_some() {
        println!("done 보고는 이 창 입력으로 들어와요. 막고 기다리려면: {wait}");
    } else {
        println!("기다리려면: {wait}");
    }
    Ok(())
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

/// `--since` 값 — epoch ms 그대로, 또는 tell 영수증 ID(`kt1.<보낸 ms>.…`)에서 보낸 시각.
fn since_ms(value: &str) -> Option<u64> {
    value.parse().ok().or_else(|| value.strip_prefix("kt1.")?.split('.').next()?.parse().ok())
}

/// 셸에 그대로 넣을 수 있게 작은따옴표로 감싼다.
/// 브리프의 「목적:」 줄 — 새로 부른 학생의 「지금 일」. 없으면 claude 가 붙이는 제목을 그대로 둔다.
fn brief_title(brief: &str) -> Option<String> {
    brief.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("목적")?.trim_start().strip_prefix(':')?;
        Some(rest.trim().to_string()).filter(|t| !t.is_empty())
    })
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `wait <이름|%N|이름@기계>… [--since ms|영수증] [--timeout 초]` — 학생들이 `done` 을 보고할 때까지 막고
/// 기다린다. 백그라운드로 돌리면 끝날 때 부른 claude 가 깨어난다. 종료 코드: 0 모두 성공 · 1 실패 보고 ·
/// 3 시간 초과 · 4 보드에서 사라짐. 사람 차례(승인·질문)는 끝이 아니라 한 줄 알리고 계속 기다린다.
///
/// `--since`(summon 이 찍어 주는 시각, 또는 tell 영수증 ID)보다 앞선 보고는 옛 브리프의 것이라 안 믿는다.
/// 없으면 건 순간 이미 쉬고 있던 학생의 보고를 옛 것으로 보고, 다시 일을 시작해 지워진 뒤의 보고만 믿는다.
fn run_wait(socket_path: &str, args: &[String]) -> Result<i32> {
    struct Watch { who: String, machine: String, key: String, stale: bool, asked: bool, missing: u32, outcome: Option<String> }
    let mut timeout = 7200u64;
    let mut since: Option<u64> = None;
    let mut targets: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--since" => {
                since = Some(args.get(i + 1).and_then(|s| since_ms(s))
                    .ok_or_else(|| anyhow!("--since 뒤에 ms 또는 tell 영수증 ID 가 필요해요"))?);
                i += 2;
            }
            "--timeout" => {
                timeout = args.get(i + 1).and_then(|s| s.parse().ok())
                    .ok_or_else(|| anyhow!("--timeout 뒤에 초가 필요해요"))?;
                i += 2;
            }
            a if a.starts_with("--") => return Err(anyhow!("모르는 옵션: {a}")),
            _ => { targets.push(arg.clone()); i += 1; }
        }
    }
    if targets.is_empty() {
        return Err(anyhow!("board --wait 는 기다릴 학생이 필요해요 — kasaterm-cli board --wait 미도리"));
    }
    let local = targets.iter().all(|t| !t.contains('@'));
    let rows = snapshot_rows(socket_path, local)?;
    let mut watches = Vec::new();
    for target in &targets {
        let row = match board_matches(&rows, target).as_slice() {
            [one] => *one,
            [] => return Err(anyhow!("「{target}」 이(가) 보드에 없어요 — `kasaterm-cli rooms` 로 이름을 확인하세요")),
            many => return Err(anyhow!("「{target}」 이(가) 여럿이에요 — 이름@기계 로 골라 주세요:\n{}",
                many.iter().map(|p| format!("  {}", describe_row(p))).collect::<Vec<_>>().join("\n"))),
        };
        let character = row_text(row, "character");
        let surface = row_address(row, "surface_id");
        watches.push(Watch {
            who: if character.is_empty() { surface.to_string() } else { format!("{character}({surface})") },
            machine: row_address(row, "machine_id").to_string(),
            key: row_address(row, "surface_key").to_string(),
            stale: since.is_none() && row.get("done_outcome").is_some() && row_text(row, "status") != "working",
            asked: false,
            missing: 0,
            outcome: None,
        });
    }
    let started = std::time::Instant::now();
    let gap = std::time::Duration::from_secs(if local { 2 } else { 5 });
    while watches.iter().any(|w| w.outcome.is_none()) {
        if started.elapsed() > std::time::Duration::from_secs(timeout) {
            for w in watches.iter().filter(|w| w.outcome.is_none()) {
                println!("{} {timeout}초 안에 done 보고 없음 — `kasaterm-cli activity` 로 확인하세요", w.who);
            }
            return Ok(3);
        }
        std::thread::sleep(gap);
        let Ok(rows) = snapshot_rows(socket_path, local) else { continue };
        for w in watches.iter_mut().filter(|w| w.outcome.is_none()) {
            let Some(row) = rows.iter().find(|p| row_address(p, "machine_id") == w.machine
                && row_address(p, "surface_key") == w.key) else {
                // 보드가 한 번 비는 것은 재시작·연결 흔들림일 수 있다 — 한참 안 보일 때만 끝으로 본다.
                w.missing += 1;
                if w.missing >= 15 {
                    println!("{} 보드에서 사라짐 — 닫혔거나 기기 연결이 끊겼어요", w.who);
                    w.outcome = Some("gone".into());
                }
                continue;
            };
            w.missing = 0;
            let status = row_text(row, "status");
            let reported = row.get("done_at_ms").and_then(|v| v.as_u64());
            let outcome = row.get("done_outcome").and_then(|v| v.as_str())
                .filter(|_| since.is_none_or(|since| reported.is_none_or(|at| at >= since)));
            if w.stale {
                if outcome.is_some() && status != "working" { continue; }
                w.stale = false;
            }
            if let Some(outcome) = outcome {
                let summary = row_text(row, "done_summary");
                println!("{} {outcome}{}", w.who, if summary.is_empty() { String::new() } else { format!(" — {summary}") });
                w.outcome = Some(outcome.to_string());
                continue;
            }
            let asking = status == "waiting" || row.get("attention_kind").is_some();
            if asking && !w.asked {
                println!("{} 사람 차례(승인·질문 대기) — 계속 기다려요", w.who);
            }
            w.asked = asking;
        }
    }
    let ended = |kind: &str| watches.iter().any(|w| w.outcome.as_deref() == Some(kind));
    Ok(if ended("gone") { 4 } else if ended("failed") { 1 } else { 0 })
}

/// 보드 스냅샷의 pane 줄. `local` 이면 이 기계만.
fn snapshot_rows(socket_path: &str, local: bool) -> Result<Vec<Value>> {
    let resp = roundtrip(socket_path, &Request {
        id: json!("snapshot"),
        method: "collab.snapshot".into(),
        params: json!({ "scope": if local { "local" } else { "all" } }),
    })?;
    resp.result.as_ref().and_then(|r| r.get("panes")).and_then(|p| p.as_array()).cloned()
        .ok_or_else(|| anyhow!("보드를 못 읽었어요 — 앱이 떠 있는지 확인"))
}

fn row_text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn row_address<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get("address").and_then(|a| a.get(key)).and_then(|v| v.as_str()).unwrap_or("")
}

/// 사람이 부르는 이름(`미도리`·`미도리@맥미니`·`%12`)에 맞는 줄 — 세션이 붙은 pane 만.
fn board_matches<'a>(rows: &'a [Value], target: &str) -> Vec<&'a Value> {
    let (name, machine) = target.rsplit_once('@').map(|(n, m)| (n, Some(m))).unwrap_or((target, None));
    // macOS 컴퓨터 이름은 띄어쓰기가 줄바꿈 없는 공백(U+00A0)이라, 목록을 보고 그대로 친 `이름@건호의 MacBook Pro`
    // 가 「보드에 없어요」로 떨어졌다(2026-10-01). 공백 종류는 가리지 않는다.
    let spaced = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let machine = machine.map(spaced);
    rows.iter().filter(|p| {
        let label = spaced(&row_text(p, "machine_label"));
        (name == row_text(p, "character") || name == row_text(p, "title") || name == row_address(p, "surface_id"))
            && machine.as_deref().is_none_or(|m| m == label || label.starts_with(m))
            && p.get("address").and_then(|a| a.get("session_id")).is_some()
    }).collect()
}

fn describe_row(p: &Value) -> String {
    format!("{}@{} · {} · {}", row_text(p, "character"), row_text(p, "machine_label"), row_text(p, "room_label"), row_text(p, "status"))
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
            "close 는 닫을 pane 을 명시해야 한다 (예: close %3 %4 [--force])"
        ));
    }
    let board = roundtrip(
        socket_path,
        &Request {
            id: "close".into(),
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
            id: "close".into(),
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
                id: "close".into(),
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
    crate::collab_root().join(enc).join("messages.jsonl")
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
        let body = g("text")
            .replace("\\n", "\n")
            .replace('\n', "\n            ");
        out.push_str(&format!("  {label}  {head}{body}\n"));
    }
    out
}

/// 방마다 칸을 행·열로, 칸 안의 탭을 한 줄씩. 찾을 말을 주면 맞는 탭만 「방 · 행·열(칸) · 탭 · 종류 · 누구·제목·주소」 한 줄로.
fn render_where(resp: &Response, query: &str, me: Option<&str>) -> String {
    let empty = Vec::new();
    let rooms = resp.result.as_ref().and_then(|v| v.get("rooms")).and_then(Value::as_array).unwrap_or(&empty);
    let kind = |k: &str| match k {
        "terminal" => "터미널",
        "web" => "웹",
        "doc" => "문서",
        "image" => "이미지",
        "settings" => "설정",
        _ => "기타",
    };
    let q = query.trim().to_lowercase();
    let mut out = Vec::new();
    for room in rooms {
        let label = room["label"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" 「{s}」")).unwrap_or_default();
        let head = format!("방 {}{label}{}", room["window"].as_u64().unwrap_or(0) + 1,
            if room["active"] == true { " (보는 중)" } else { "" });
        let mut lines = Vec::new();
        for cell in room["cells"].as_array().unwrap_or(&empty) {
            let tabs = cell["tabs"].as_array().unwrap_or(&empty);
            let at = format!("{}행 {}열({})", cell["row"], cell["col"], cell["cell"].as_str().unwrap_or("?"));
            for tab in tabs {
                let surface = tab["surface"].as_str().unwrap_or("");
                let about = [tab["character"].as_str(), tab["title"].as_str(), tab["url"].as_str(), tab["path"].as_str()]
                    .into_iter().flatten().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let tab_at = if tabs.len() > 1 {
                    format!(" · 탭 {}/{}{}", tab["n"], tabs.len(), if tab["active"] == true { "" } else { "(뒤)" })
                } else {
                    String::new()
                };
                let mine = me.is_some_and(|m| !surface.is_empty() && m == surface);
                let what = kind(tab["kind"].as_str().unwrap_or(""));
                let what = if surface.is_empty() { what.to_string() } else { format!("{what} {surface}") };
                let line = format!("{at}{tab_at} · {what}{}{}",
                    if about.is_empty() { String::new() } else { format!(" · {about}") },
                    if mine { "  ← 나" } else { "" });
                if q.is_empty() {
                    lines.push(format!("  {line}"));
                } else if format!("{head} {line}").to_lowercase().contains(&q) {
                    out.push(format!("{head} · {line}"));
                }
            }
        }
        if q.is_empty() {
            out.push(head);
            let rects: Vec<(String, u16, u16, u16, u16)> = room["cells"].as_array().unwrap_or(&empty).iter()
                .filter_map(|c| Some((c["cell"].as_str()?.to_string(), c["x"].as_u64()? as u16, c["y"].as_u64()? as u16,
                    c["w"].as_u64()? as u16, c["h"].as_u64()? as u16)))
                .collect();
            if !rects.is_empty() {
                out.extend(draw_boxes(&rects).lines().map(|l| format!("  {l}")));
            }
            out.extend(lines);
        }
    }
    match (out.is_empty(), q.is_empty()) {
        (false, _) => out.join("\n"),
        (true, true) => "(방이 없어요)".into(),
        (true, false) => format!("「{query}」에 맞는 칸·탭이 없어요"),
    }
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

fn share_local_date() -> String {
    #[cfg(unix)]
    {
        let now = unsafe { libc::time(std::ptr::null_mut()) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&now, &mut tm) };
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
    }
    #[cfg(windows)]
    {
        let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
        format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
    }
}

/// 폴더 이름에 못 쓰는 글자를 `-` 로. 윈도우에서도 같은 이름이어야 옮겨진다.
fn share_topic(words: &[String]) -> String {
    let raw = words.join(" ");
    let clean: String = raw
        .chars()
        .map(|c| if c < ' ' || "/\\<>:\"|?*".contains(c) { '-' } else { c })
        .collect();
    clean.trim().trim_matches('.').trim().to_string()
}

fn run_share(args: &[String]) -> Result<Option<Response>> {
    let root = crate::share_dir().ok_or_else(|| anyhow!("홈 폴더를 못 찾았다"))?;
    let status: Value = std::fs::read_to_string(root.join(".kasaterm").join("status.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    // 사람에게 보일 경로는 바탕화면 링크 쪽 — 파인더에서 그 이름으로 보인다.
    let shown = status["desktop_link"]
        .as_str()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or_else(|| root.clone());
    match args.first().map(String::as_str).unwrap_or("path") {
        "path" => println!("{}", shown.display()),
        "new" => {
            let topic = share_topic(&args[1..]);
            if topic.is_empty() {
                return Err(anyhow!("share new <주제> — 주제가 필요하다"));
            }
            let name = format!("{}-{topic}", share_local_date());
            std::fs::create_dir_all(root.join(&name))?;
            println!("{}", shown.join(&name).display());
        }
        "status" => {
            if status.is_null() {
                println!("KASA-share: 아직 안 돌았다 — 새 판 카사텀이 켜져 있어야 한다 ({})", root.display());
                return Ok(None);
            }
            let ago = crate::board::now_ms().saturating_sub(status["updated_ms"].as_u64().unwrap_or(0)) / 1000;
            println!(
                "KASA-share {} · 파일 {}개 · {}초 전 갱신",
                shown.display(),
                status["files"].as_u64().unwrap_or(0),
                ago
            );
            for p in status["peers"].as_array().into_iter().flatten() {
                let state = if p["ok"] == true {
                    format!("맞춤 (이번에 {}개)", p["pulled"].as_u64().unwrap_or(0))
                } else {
                    p["error"].as_str().unwrap_or("실패").to_string()
                };
                println!("  {} — {state}", p["label"].as_str().unwrap_or("?"));
            }
            let paused = status["paused_deletes"].as_u64().unwrap_or(0);
            if paused > 0 {
                println!("  삭제 {paused}개를 멈췄다 — 한꺼번에 많이 사라졌다. 맞으면 `kasaterm-cli share accept-deletes`");
            }
            for (key, what) in [("too_big", "1GB 넘어 이 기기에만"), ("skipped", "이 기기에 둘 수 없는 이름")] {
                for p in status[key].as_array().into_iter().flatten() {
                    println!("  {what}: {}", p.as_str().unwrap_or(""));
                }
            }
        }
        "accept-deletes" => {
            std::fs::create_dir_all(root.join(".kasaterm"))?;
            std::fs::write(root.join(".kasaterm").join("accept-deletes"), b"")?;
            println!("다음 훑기에 멈춘 삭제를 퍼뜨린다");
        }
        other => return Err(anyhow!("share {other}? — path | new <주제> | status | accept-deletes")),
    }
    Ok(None)
}

fn print_help() {
    // 용도별로 묶는다 — 명령이 60개를 넘자 한 줄 목록에서는 무엇을 써야 할지 못 찾았다(2026-09-29 CLI 정리).
    let groups: &[(&str, &[&str])] = &[
        ("보기 — 누가 무엇을 하나, 어디 있나", &[
            "where [찾을 말] [--json]                  방마다 칸 배치도 + 칸·탭 목록. 학생 이름·%N·제목·웹 주소·문서 경로로 찾는다",
            "board [--all|--local]                     학생 상태. 연락 주소(address)는 --all 에서",
            "board --wait <이름|%N>… [--since ms|영수증] [--timeout 초]   done 보고까지 기다린다(0 성공·1 실패·3 시간초과·4 사라짐)",
            "board-watch --all --json [--since CURSOR] 바뀐 것만 흘려보낸다(Monitor 용)",
            "peek [%N] [줄수]                          pane 화면 글자",
            "capture [%N] [경로] | --window [경로]      pane 또는 창 전체 스크린샷",
            "transcript [%N] [N]                       claude 최근 대화 N 턴",
            "activity [%N | --address JSON] [N]        실제로 한 도구·인자·결과(시간순)",
        ]),
        ("학생·협업", &[
            "tell <이름|이름@기계|%N|--address JSON> [--title \"지금 일\"] <글|--stdin>   안전 전달, 영수증 ID 를 준다. 새 일이면 --title",
            "tell --status ID                          전달 영수증 조회",
            "tell --raw [%N] <글> · tell --key [%N] <enter|tab|escape|up|ctrl+x|alt+b|…>   안전장치 없이 바로 넣기(셸·승인 창용)",
            "summon [--cwd 폴더] [--tab] [--name 제목] <브리프|--stdin>   학생을 옆에 세우고 브리프까지",
            "done <succeeded|failed|blocked|needs_restart|needs_approval> [요약] [--changed 파일]… [--tests 글] [--next 글]",
            "                                          내 일 보고. 오케스트레이터가 띄운 창이면 그쪽 보고함에도 넣는다",
            "sessions [N]                              최근 claude·codex 세션 목록",
        ]),
        ("창·칸 조작", &[
            "split <left|right|up|down> [%N] [--focus] [--count N]   칸 나누기 — 방 전체가 같은 크기 격자로 다시 짜인다",
            "split <방향> %N@기계 · tab %N@기계        다른 기기 칸 옆·탭에 세우기",
            "tab [%N] [--focus]                        그 칸에 새 탭",
            "tab --server --surface %N [--cwd 폴더] [--name 라벨] '<명령>' | --clear   서버 실행·복원 등록(비밀값 금지)",
            "window-new [--machine 기계]               새 방(그 기계에 만들고 여기서 보기)",
            "move %N <대상> [방향] · resize %N <0..1>  칸 옮기기 · 비율",
            "focus %N · closed [%N]                    포커스 · 되살리기 목록(%N 을 주면 진짜 끈다)",
            "close %N… [--force]                       칸 닫기(미커밋 변경이 있으면 안 닫는다)",
            "rename-window [%N] <이름> | rename-window [%N] --color #rrggbb   %N 이면 그 칸 이름·색, 없으면 방 이름",
        ]),
        ("클립보드·결과물", &[
            "copy <글> | copy --surface %N [줄수] | copy --secret(표준입력)   클립보드에 넣기",
            "paste [--show] | paste --into [%N] | paste --env VAR -- <명령…>   읽기 · 값을 안 보고 붙이기 · 환경변수로",
            "share path | new <주제> | status          결과물 폴더(new 는 경로를 찍는다)",
            "share open <url>                          사람이 보는 브라우저로(어느 기기인지는 사람이 고른다)",
        ]),
        ("기기·네트워크", &[
            "machines [--names]                        명부 기계 목록",
            "machines connect <기기|http://호스트:포트> [--here] [--cwd 경로] [--run 명령]   그 기기에 새 방(보기 창은 뒤에) · --here 면 이 칸 자리를 그 기기 셸로",
            "machines move [%N] <기기|local> [--cwd /레포] [--force]   칸의 claude 를 그 기기로 이사(대화·미커밋 변경까지)",
            "net forward <기기> <port> [--local L] · net list · net stop <L>   다른 기기 포트 끌어오기",
        ]),
        ("계정 일 권한 (설정 → 계정에서 연결한 Gmail·GitHub)", &[
            "mail [list] [--query 'is:unread'] [--max N] · mail read <id>   메일 목록·본문",
            "mail send --to a@x[,b@y] [--cc …] --subject 제목 --body 본문|- [--reply-to <id>]   보내기 요청(사람이 앱에서 승인)",
            "pr create --repo 주인/레포 --head 브랜치 [--base main] --title 제목 [--body 본문|-] [--draft]   PR 요청(사람이 앱에서 승인)",
            "mail connections                          연결·승인 대기 목록  (--connection <id> 로 연결 고르기)",
        ]),
        ("1Password (폰 Face ID 한 번으로 그 요청만)", &[
            "op run -e 이름=op://금고/항목/필드 [-e …] -- <명령…>   값은 그 명령의 환경으로만, 출력에 나오면 가림",
            "op read op://금고/항목/필드               값을 표준출력으로(끝 줄바꿈 없음) — x=$(…) 로 담아 쓴다",
            "op status                                 토큰·금고·믿는 폰 열쇠(값 없음)",
        ]),
        ("앱", &[
            "app-update run|start|status …            기기 앱 업데이트(공식 릴리스·승인 필요)",
            "app-restart plan|run|status …            기기 앱 재시작 계획·실행",
        ]),
        ("훅 (claude 훅이 부른다)", &[
            "bind-transcript <path> · notify [--surface %N] <제목> [본문] · attention [--surface %N] [사유]",
            "agent-status <start|end|clear> <subagent|background> [key] [라벨] · identify(내 칸 번호)",
        ]),
    ];
    eprintln!("kasaterm-cli — 카사텀 조작 CLI. 대상은 %N(칸 번호)이나 학생 이름.\n");
    for (title, lines) in groups.iter() {
        eprintln!("{title}");
        for line in lines.iter() {
            eprintln!("  {line}");
        }
        eprintln!();
    }
    eprintln!("앞에 붙이는 것: --api BASE [--api-token-file FILE] — 다른 기기 HTTP 로 보낸다");
    eprintln!("소켓: $KASATERM_SOCKET_PATH > $CMUX_SOCKET_PATH > 기본(/tmp/cmux.sock, Windows \\\\.\\pipe\\cmux)");
}

/// `--flag 값` 꼴의 값.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).filter(|v| !v.starts_with("--")).cloned()
}

/// 다른 기계의 pane 을 가리키는 인자 — `%N@기계`, 또는 `%N` + `--machine 기계`. `(pane, 기계)`.
fn remote_target(args: &[String]) -> Option<(String, String)> {
    if let Some((pane, machine)) = args.iter().find(|a| a.starts_with('%')).and_then(|a| a.split_once('@')) {
        if !machine.is_empty() { return Some((pane.to_string(), machine.to_string())); }
    }
    let machine = flag_value(args, "--machine")?;
    let pane = args.iter().find(|a| a.starts_with('%') && !a.contains('@'))?;
    Some((pane.clone(), machine))
}

fn server_params(args: &[String]) -> Result<Value> {
    let mut params = json!({});
    let mut clear = false;
    let mut register_only = false;
    let mut command = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--surface" | "--cwd" | "--name" => {
                let value = args
                    .next()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| anyhow!("server option needs a nonempty value"))?;
                params[arg.trim_start_matches('-')] = json!(value);
            }
            "--register-only" => register_only = true,
            "--clear" => clear = true,
            "--" => {
                command = args.next().cloned();
                if args.next().is_some() {
                    return Err(anyhow!("server command must be quoted as one argument"));
                }
                break;
            }
            _ if arg.starts_with('-') => return Err(anyhow!("unknown server option")),
            _ => {
                if command.replace(arg.clone()).is_some() {
                    return Err(anyhow!("server command must be quoted as one argument"));
                }
            }
        }
    }
    if params.get("surface").is_none() {
        return Err(anyhow!(
            "server requires --surface to identify the server pane"
        ));
    }
    if clear {
        if command.is_some()
            || register_only
            || params.get("cwd").is_some()
            || params.get("name").is_some()
        {
            return Err(anyhow!("server --clear accepts only --surface"));
        }
        params["clear"] = json!(true);
    } else {
        let command = command
            .filter(|command| !command.trim().is_empty())
            .ok_or_else(|| anyhow!("server requires one quoted command"))?;
        // Joining argv would silently strip quoting before the restored shell runs it.
        params["command"] = json!(command);
        params["start"] = json!(!register_only);
    }
    Ok(params)
}

/// `done` 인자를 판 완료(`surface.done`)용과 오케스트레이터 보고용으로 가른다. 판은 성공·실패 둘만 받으므로
/// 막힘·재시작·승인은 실패로 적되 요약 앞에 사연을 붙인다.
fn split_done_args(args: &[String]) -> Result<(Vec<String>, Vec<String>)> {
    let (mut board, mut report, mut words) = (Vec::new(), Vec::new(), Vec::new());
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        match flag {
            // --task·--conv·--machine·--run 은 오케스트레이터 브리프가 첫 줄에 적어 주는 것이다.
            "--surface" | "--changed" | "--tests" | "--next" | "--task" | "--conv" | "--machine" | "--run" => {
                let value = args.get(i + 1).ok_or_else(|| anyhow!("{flag} needs a value"))?.clone();
                let into = if flag == "--surface" { &mut board } else { &mut report };
                into.extend([flag.to_string(), value]);
                i += 2;
            }
            _ => {
                words.push(args[i].clone());
                i += 1;
            }
        }
    }
    let status = words.first().map(String::as_str).unwrap_or("");
    let (outcome, reported, note) = match status {
        "succeeded" | "success" | "ok" => ("succeeded", "done", ""),
        "failed" | "fail" => ("failed", "blocked", ""),
        "blocked" => ("failed", "blocked", "막힘: "),
        "needs_restart" => ("failed", "needs_restart", "재시작 필요: "),
        "needs_approval" => ("failed", "needs_approval", "승인 필요: "),
        "" => return Err(anyhow!("done needs <succeeded|failed|blocked|needs_restart|needs_approval> [한 줄 요약]")),
        other => return Err(anyhow!(
            "done 상태는 succeeded|failed|blocked|needs_restart|needs_approval 중 하나, 받은 것 \"{other}\""
        )),
    };
    let summary = words[1..].join(" ");
    board.push(outcome.to_string());
    if !note.is_empty() || !summary.is_empty() {
        board.push(format!("{note}{summary}"));
    }
    let summary = if summary.is_empty() { reported.to_string() } else { summary };
    report.extend(["--status".to_string(), reported.to_string(), "--summary".to_string(), summary]);
    Ok((board, report))
}

/// `--body -` reads the text from standard input so long mail and PR bodies need no quoting.
fn body_flag(args: &[String]) -> Result<Option<String>> {
    match flag_value(args, "--body").as_deref() {
        Some("-") => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).context("표준입력 읽기")?;
            Ok(Some(text))
        }
        other => Ok(other.map(str::to_string)),
    }
}

fn address_list(args: &[String], flag: &str) -> Vec<String> {
    flag_value(args, flag)
        .map(|list| list.split(',').map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect())
        .unwrap_or_default()
}

fn build_request(cmd: &str, args: &[String]) -> Result<Request> {
    // Caller-supplied id so async clients can correlate; we just stamp
    // a process-id-based string for the CLI path where nobody cares.
    let id = json!(format!("cli-{}", std::process::id()));
    let (method, params): (&str, Value) = match cmd {
        // 계정 일 권한(docs/account-connections.md) — 관문이 대신 부르고, 쓰기는 사람이 앱 화면에서 승인할 때까지 기다린다.
        "mail" => {
            let connection = flag_value(args, "--connection");
            match args.first().map(String::as_str) {
                Some("list") | None => ("relay.account", json!({ "op": "mail_list", "connection": connection,
                    "query": flag_value(args, "--query").unwrap_or_default(),
                    "max": flag_value(args, "--max").and_then(|m| m.parse::<u64>().ok()) })),
                Some("read") => {
                    let message = args.get(1).filter(|a| !a.starts_with("--")).ok_or_else(|| anyhow!("mail read <id>"))?;
                    ("relay.account", json!({ "op": "mail_read", "connection": connection, "id": message }))
                }
                Some("send") => {
                    let (to, subject, body) = (address_list(args, "--to"), flag_value(args, "--subject"), body_flag(args)?);
                    let (Some(subject), Some(body)) = (subject, body) else {
                        return Err(anyhow!("mail send --to a@x --subject 제목 --body 본문|-"));
                    };
                    ("relay.account", json!({ "op": "mail_send", "connection": connection, "to": to,
                        "cc": address_list(args, "--cc"), "subject": subject, "body": body,
                        "reply_to": flag_value(args, "--reply-to") }))
                }
                Some("connections") => ("relay.account", json!({ "op": "connections" })),
                Some(other) => return Err(anyhow!("mail list|read|send|connections — 모르는 것: {other}")),
            }
        }
        "pr" => match args.first().map(String::as_str) {
            Some("create") => {
                let (repo, head, title) = (flag_value(args, "--repo"), flag_value(args, "--head"), flag_value(args, "--title"));
                let (Some(repo), Some(head), Some(title)) = (repo, head, title) else {
                    return Err(anyhow!("pr create --repo 주인/레포 --head 브랜치 --title 제목 [--base main] [--body 본문|-] [--draft]"));
                };
                ("relay.account", json!({ "op": "pr_create", "connection": flag_value(args, "--connection"),
                    "repo": repo, "head": head, "base": flag_value(args, "--base").unwrap_or_else(|| "main".into()),
                    "title": title, "body": body_flag(args)?.unwrap_or_default(),
                    "draft": args.iter().any(|a| a == "--draft") }))
            }
            _ => return Err(anyhow!("pr create --repo 주인/레포 --head 브랜치 --title 제목")),
        },
        // 카사넷 포트 공유 — 다른 기기 개발 서버를 이 기기 localhost 로 끌어온다(docs/kasanet.md P3).
        "net" => match args.first().map(String::as_str) {
            Some("forward") => {
                let positional: Vec<&String> = {
                    let mut out = Vec::new();
                    let mut i = 1;
                    while i < args.len() {
                        if args[i] == "--local" {
                            i += 2;
                            continue;
                        }
                        out.push(&args[i]);
                        i += 1;
                    }
                    out
                };
                let (Some(machine), Some(port)) = (positional.first(), positional.get(1)) else {
                    return Err(anyhow!("net forward <기기> <port> [--local L]"));
                };
                let port: u16 = port.parse().map_err(|_| anyhow!("포트는 1~65535 숫자: {port}"))?;
                let local = match flag_value(args, "--local") {
                    Some(l) => Some(l.parse::<u16>().map_err(|_| anyhow!("--local 은 1~65535 숫자: {l}"))?),
                    None => None,
                };
                ("net.forward", json!({ "op": "forward", "machine": machine, "port": port, "local": local }))
            }
            Some("list") | None => ("net.forward", json!({ "op": "list" })),
            Some("stop") => {
                let local = args.get(1).ok_or_else(|| anyhow!("net stop <로컬 포트>"))?;
                let local: u16 = local.parse().map_err(|_| anyhow!("포트는 1~65535 숫자: {local}"))?;
                ("net.forward", json!({ "op": "stop", "local": local }))
            }
            Some(other) => return Err(anyhow!("net forward|list|stop — 모르는 것: {other}")),
        },
        "identify" => ("system.identify", json!({})),
        "focus" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("focus needs a surface_id"))?;
            ("surface.focus", json!({ "surface_id": surface }))
        }
        // rename-window [%N] <이름> | rename-window [%N] --color #rrggbb — %N 을 주면 그 칸의 이름(고정 —
        // claude 가 붙이는 제목이 못 덮는다)·색, 없으면 이 칸이 속한 방의 이름. 제목에 공백이 있으면 따옴표로.
        "rename-window" => {
            let pane_like = |s: &str| {
                s.strip_prefix('%')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            };
            let pane = args.first().filter(|a| pane_like(a)).cloned();
            let rest = if pane.is_some() { &args[1..] } else { &args[..] };
            if rest.first().is_some_and(|a| a == "--color") {
                let surface = pane.or_else(|| std::env::var("KASATERM_PANE_ID").ok()).ok_or_else(|| {
                    anyhow!("rename-window --color needs %N or $KASATERM_PANE_ID")
                })?;
                let color = rest.get(1).ok_or_else(|| anyhow!("--color needs a #rrggbb value"))?;
                ("surface.set_color", json!({ "surface_id": surface, "color": color }))
            } else {
                // 남는 인자를 조용히 버리지 않는다 — 따옴표를 빠뜨려 두 토막 난 제목이 앞 토막만 먹힌다.
                if rest.len() != 1 {
                    return Err(anyhow!("rename-window [%N] <이름> — 이름은 하나(공백이 있으면 따옴표로)"));
                }
                match pane {
                    Some(surface) => ("surface.rename", json!({ "surface_id": surface, "title": rest[0] })),
                    None => {
                        let surface = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                            anyhow!("rename-window needs %N or $KASATERM_PANE_ID (run inside a kasaterm pane)")
                        })?;
                        ("window.rename", json!({ "surface_id": surface, "title": rest[0] }))
                    }
                }
            }
        }
        "repersona" => {
            // 이 pane 의 다음 claude 가 쓸 캐릭터를 갈아끼운다(respawn 없음). 이름은
            // 활성 테마 밖이어도 된다 — 설치 테마까지 합쳐 찾으므로, 오케스트레이터 전용 테마를
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
            // 상태줄(`statusline`)이 값이 바뀔 때마다 호출:
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
            let cwd = args
                .get(1)
                .ok_or_else(|| anyhow!("report-cwd needs a cwd"))?;
            let session_id = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let ctx_window: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let ctx_tokens: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
            let model = args.get(5).map(|s| s.as_str()).unwrap_or("");
            let effort = args.get(6).map(|s| s.as_str()).unwrap_or("");
            // 여덟째: 상태줄에 찍는 모델 표시명("Opus 4.8 1M"). 보드가 화면 대신 이걸 쓴다.
            let model_label = args.get(7).map(|s| s.as_str()).unwrap_or("");
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
                    "model_label": model_label,
                }),
            )
        }
        "split" => {
            // `%N@기계` 또는 `--machine 기계` — 저쪽 그 pane 옆에 세운다(축은 저쪽이 고른다).
            if let Some(remote) = remote_target(args) {
                return Ok(Request { id, method: "remote.spawn_shell".into(),
                    params: json!({ "machine": remote.1, "beside": remote.0, "cwd": flag_value(args, "--cwd") }) });
            }
            // 기본 no-focus(자동화: tell 처럼 포커스 안 뺏음). --focus 로 옵트인.
            let focus = args.iter().any(|a| a == "--focus");
            // 방향은 **선택**이다 — 생략하면 `auto`, 즉 앱이 pane 의 종횡비를 보고 긴
            // 축을 쪼갠다(사용자 2026-08-05: "너무 가로로나 세로로 안 길게"). 사람이
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
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                });
            (
                "surface.split",
                json!({ "direction": dir, "focus": focus, "from": from }),
            )
        }
        // 새 창(사이드바에 하나 더). 창 간 이동(`move`)의 목적지를 만들 때 쓴다.
        "window-new" => {
            // `--machine 기계` — 저쪽에 새 방을 만들고 여기 보기 창으로 연다.
            if let Some(machine) = flag_value(args, "--machine") {
                return Ok(Request { id, method: "remote.spawn_shell".into(),
                    params: json!({ "machine": machine, "window": "new", "cwd": flag_value(args, "--cwd") }) });
            }
            ("window.new", json!({}))
        }
        "tab:server" => ("surface.server", server_params(args)?),
        // 원격 PTY 호스트(kasa-serve-web)의 셸을 pane 으로 — 학생을 맥미니에서
        // 돌리고 이 창은 미러다. 앱을 꺼도(detach) 원격 셸은 살아남고, 재시작하면
        // 같은 세션에 다시 붙는다.
        // share open — 사람이 보는 브라우저로(칸 안 웹 pane 이 아니다). 어느 기기로 갈지는 사람이 고른다.
        "share:open" => {
            let url = args
                .iter()
                .find(|a| !a.starts_with('%'))
                .ok_or_else(|| anyhow!("share open needs a URL (e.g. share open https://example.com)"))?;
            let target = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                });
            ("surface.open_url", json!({ "url": url, "target": target }))
        }
        // machines move — 칸의 claude 를 다른 기계로 이사(대화·미커밋 변경까지 운반, 같은 자리에서 재개).
        "machines:move" => {
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
                anyhow!("machines move 는 목적지가 필요해요 (예: machines move 맥미니 · machines move %3 맥미니 · 데려오기: machines move %3 local)")
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
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                })
                .ok_or_else(|| {
                    anyhow!("machines move 는 대상 pane 이 필요해요 (예: machines move %3 맥미니)")
                })?;
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
        "machines:connect" => {
            // 플래그 값(--cwd /x)이 base 로 오인되지 않게 위치 인자만 걷는다.
            let mut positional: Vec<String> = Vec::new();
            let mut i = 0usize;
            while i < args.len() {
                let a = &args[i];
                if a == "--cwd" || a == "--attach" || a == "--run" {
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
                anyhow!("machines connect 는 호스트 주소나 기계 이름이 필요해요 (예: machines connect 맥미니 --here · machines connect 맥미니 --here --run codex · machines connect http://127.0.0.1:18766 --cwd /Users/me)")
            })?;
            // `--here` — 옆에 쪼개지 않고 기준 pane 자체를 거울로 갈아끼운다. 그 pane
            // 의 셸에서 부른 것이면 이 프로세스도 셸과 함께 걷히므로 회신은 안 온다.
            let here = args.iter().any(|a| a == "--here");
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
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                });
            // 붙기 전에 빌드를 견준다 — 저쪽 프로그램이 다른 판이면 새 창구가 없어
            // 창 없는 셸로 물러서거나 조용히 어긋난다. 막지는 않고 한 줄만 알린다.
            if let Ok(sp) = resolve_socket_path() {
                if let Ok(resp) = roundtrip(
                    &sp,
                    &Request {
                        id: "machines".into(),
                        method: "machine.list".into(),
                        params: json!({ "from": from }),
                    },
                ) {
                    let rows = resp
                        .result
                        .and_then(|r| r.get("machines").and_then(|v| v.as_array()).cloned())
                        .unwrap_or_default();
                    if let Some(m) = rows
                        .iter()
                        .find(|m| m.get("label").and_then(|v| v.as_str()) == Some(base.as_str()))
                    {
                        let online = m.get("online").and_then(|v| v.as_bool()).unwrap_or(false);
                        let same = m.get("build_match").and_then(|v| v.as_bool()).unwrap_or(true);
                        if online && !same {
                            eprintln!(
                                "⚠ {base} 의 카사텀 빌드가 이쪽과 달라요({}) — 새 판을 부치세요(scripts/sync-mini.sh)",
                                m.get("build").and_then(|v| v.as_str()).unwrap_or("옛 판, 표식 없음")
                            );
                        }
                    }
                }
            }
            (
                "surface.remote",
                json!({ "base": base, "cwd": flagval("--cwd"), "pane": flagval("--attach"), "from": from, "here": here, "run": flagval("--run") }),
            )
        }
        // 쪼개지 않고 **이 pane 안에** 새 탭. 학생을 더 띄워도 화면이 안 줄어든다.
        // 기본은 no-focus — 부모(부른 쪽) 화면이 그대로 남는다. --focus 만 새 탭을
        // 앞으로 올린다(split 의 --focus 와 같은 규약).
        "tab" => {
            if let Some(remote) = remote_target(args) {
                return Ok(Request { id, method: "remote.spawn_shell".into(),
                    params: json!({ "machine": remote.1, "tab_of": remote.0, "cwd": flag_value(args, "--cwd") }) });
            }
            let focus = args.iter().any(|a| a == "--focus");
            let outer = args
                .iter()
                .find(|a| a.starts_with('%'))
                .cloned()
                .or_else(|| {
                    std::env::var("KASATERM_PANE_ID")
                        .ok()
                        .filter(|s| !s.is_empty())
                });
            ("surface.new_tab", json!({ "outer": outer, "focus": focus }))
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
        "resize" => {
            let surface = args
                .first()
                .ok_or_else(|| anyhow!("resize needs <surface_id> <ratio>"))?;
            let ratio: f64 = args
                .get(1)
                .ok_or_else(|| anyhow!("resize needs a ratio (0..1)"))?
                .parse()
                .map_err(|_| anyhow!("ratio must be a number, e.g. 0.6"))?;
            (
                "surface.set_ratio",
                json!({ "surface_id": surface, "ratio": ratio }),
            )
        }
        // tell --raw / --key — 안전장치(빈 입력창 기다리기·신원 확인) 없이 글·키를 바로 넣는다. 셸이나
        // 승인 창처럼 tell 이 못 받는 곳에 쓴다. 대상은 %N 이나 --surface %N, 없으면 보고 있는 칸.
        "tell:raw" | "tell:key" => {
            let (surface, rest) = match args.first().map(String::as_str) {
                Some("--surface") => (
                    Some(args.get(1).ok_or_else(|| anyhow!("--surface needs an id"))?.clone()),
                    args.get(2..).unwrap_or(&[]),
                ),
                Some(a) if a.starts_with('%') => (Some(a.to_string()), args.get(1..).unwrap_or(&[])),
                _ => (None, &args[..]),
            };
            let what = if cmd == "tell:raw" { "text" } else { "key" };
            let value = rest.join(" ");
            if value.is_empty() {
                return Err(anyhow!("tell --{} needs a {what}", &cmd[5..]));
            }
            let mut params = json!({ what: value });
            if let Some(s) = surface {
                params["surface_id"] = json!(s);
            }
            (if cmd == "tell:raw" { "surface.send_text" } else { "surface.send_key" }, params)
        }
        "tell" => {
            let mut index = 0;
            let mut stdin = false;
            let mut params = json!({"message_id":crate::tell::new_message_id()});
            while let Some(arg) = args.get(index) {
                match arg.as_str() {
                    "--id" => {
                        params["message_id"] = json!(args.get(index+1).ok_or_else(||anyhow!("--id needs a message ID"))?);
                        index += 2;
                    }
                    "--address" => {
                        params["address"] = serde_json::from_str(args.get(index+1).ok_or_else(||anyhow!("--address needs complete board address JSON"))?)?;
                        index += 2;
                    }
                    "--stdin" => { stdin = true; index += 1; }
                    "--title" => {
                        let title = args.get(index+1).ok_or_else(||anyhow!("--title needs the receiver's current work"))?;
                        params["title"] = json!(crate::tell::normalize_title(title)?);
                        index += 2;
                    }
                    "--force" => return Err(anyhow!("--force cannot bypass safe tell protection")),
                    "--" => { index += 1; break; }
                    _ if arg.starts_with('%') && params.get("address").is_none() && params.get("surface_id").is_none() => {
                        params["surface_id"] = json!(arg); index += 1;
                    }
                    // 모르는 옵션을 본문으로 넘기면 받는 쪽에는 옵션 줄이 가고 진짜 본문은 빠진다(2026-10-01
                    // `--title` 을 모르던 판이 그랬다). `--` 로 시작하는 본문은 `--` 뒤에 쓴다.
                    _ if arg.starts_with("--") => return Err(anyhow!(
                        "tell: 모르는 옵션 {arg} (옵션: --title --stdin --id --address). --로 시작하는 본문이면 `--` 뒤에 쓰세요")),
                    _ => break,
                }
            }
            if let Some(flag) = args.get(index..).unwrap_or_default().iter().find(|a|matches!(a.as_str(),"--title"|"--stdin"|"--id"|"--address")) {
                return Err(anyhow!("tell: {flag} 가 본문 뒤에 있어요 — 옵션은 대상 바로 뒤, 본문 앞에 두세요"));
            }
            if params.get("address").is_none() && params.get("surface_id").is_none() {
                return Err(anyhow!("tell requires %surface or --address JSON"));
            }
            if params.get("address").is_some() && params.get("surface_id").is_some() {
                return Err(anyhow!("use one target: %surface or --address JSON"));
            }
            let body = if stdin {
                if index != args.len() { return Err(anyhow!("--stdin cannot be combined with a message argument")); }
                use std::io::Read;
                let mut body = String::new();
                std::io::stdin().take(crate::tell::MAX_BODY as u64 + 1).read_to_string(&mut body)?;
                body
            } else {
                args.get(index..).filter(|a|!a.is_empty()).ok_or_else(||anyhow!("tell needs a message or --stdin"))?.join(" ")
            };
            // 발신 학생 마커 — 받는 pane 이 tell 을 발신자 테마색·프사로 그리려면 화면에
            // 앵커가 필요하다(그리드라 transcript 로 user 턴을 못 집는다). 옛 tell 이 심던
            // `⟦이름⟧` 을 collab.tell 로 옮기며 빠뜨려, 남이 보낸 tell 이 사용자 발신처럼
            // 무테마로 떴다(2026-09-17 지적 「tell 로 보내면 학생 프사 나오면서 그거 왜 안 되지」).
            // 사람이 직접 친 cli 는 env 가 없어 마커 없이(사용자 발신=무색) 나간다.
            let body = mark_tell_sender(body, std::env::var("KASATERM_CHARACTER").ok().as_deref());
            params["body"] = json!(crate::tell::normalize(&body)?);
            crate::tell::valid_id(params["message_id"].as_str().unwrap())?;
            eprintln!("tell receipt ID: {}",params["message_id"].as_str().unwrap());
            ("collab.tell",params)
        }
        "tell:status" => {
            let id = args.first().ok_or_else(||anyhow!("tell --status needs message_id [--address JSON]"))?;
            if args.get(1).is_none_or(|a|a != "--address") { return Err(anyhow!("tell --status needs the original --address JSON")); }
            let address: Value = serde_json::from_str(args.get(2).ok_or_else(||anyhow!("missing receipt address"))?)?;
            ("collab.tell_status",json!({"message_id":id,"address":address}))
        }
        "recent-sessions" => {
            // recent-sessions [cwd] — 이어갈 후보 세션 목록(최신순, id/label/mtime/cwd). tell
            // 오발송(없는 학생) 시 사라진 학생 세션을 찾아 resume 하는 데 쓴다(사용자: 내가 자동).
            let cwd = args.first().filter(|s| !s.is_empty()).cloned();
            ("session.recent", json!({ "cwd": cwd }))
        }
        "board" => {
            if args.iter().any(|s|matches!(s.as_str(),"--all"|"--local")) {
                if args.iter().any(|s|!matches!(s.as_str(),"--all"|"--local"|"--json")) {
                    return Err(anyhow!("board scope accepts only --all, --local and --json"));
                }
                let scope = if args.iter().any(|s|s == "--local") {"local"} else {"all"};
                return Ok(Request{id,method:"collab.snapshot".into(),params:json!({"scope":scope})});
            }
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
                    let s = std::env::var("KASATERM_PANE_ID")
                        .map_err(|_| anyhow!("notify needs --surface <id> or $KASATERM_PANE_ID"))?;
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
            // --kind permission|question|idle — 무엇을 기다리는지(Notification 훅의 종류).
            let mut kind = String::new();
            let mut words: Vec<String> = Vec::new();
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "--kind" {
                    kind = it.next().cloned().unwrap_or_default();
                } else {
                    words.push(a.clone());
                }
            }
            let reason = words.join(" ");
            (
                "surface.attention",
                json!({ "surface_id": surface, "reason": reason, "kind": kind }),
            )
        }
        "turn" => {
            let (surface, rest): (String, &[String]) =
                if args.first().is_some_and(|a| a == "--surface") {
                    let s = args
                        .get(1)
                        .ok_or_else(|| anyhow!("--surface needs an id"))?
                        .clone();
                    (s, args.get(2..).unwrap_or(&[]))
                } else {
                    let s = std::env::var("KASATERM_PANE_ID").map_err(|_| {
                        anyhow!("turn needs --surface <id> or $KASATERM_PANE_ID")
                    })?;
                    (s, &args[..])
                };
            let phase = rest
                .first()
                .ok_or_else(|| anyhow!("turn needs <start|end|compact_start|compact_end|reset>"))?
                .clone();
            let mode = rest
                .iter()
                .position(|a| a == "--permission-mode")
                .and_then(|i| rest.get(i + 1))
                .cloned()
                .unwrap_or_default();
            (
                "surface.turn",
                json!({ "surface_id": surface, "phase": phase, "permission_mode": mode }),
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
                    let s = std::env::var("KASATERM_PANE_ID")
                        .map_err(|_| anyhow!("done needs --surface <id> or $KASATERM_PANE_ID"))?;
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
        "where" => ("window.where", json!({})),
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
        "copy" => {
            // 캐릭터가 사람 대신 복사해 주는 문. 사람이 직접 끌어서 복사하는 길은
            // 노플리커를 끈 claude 앞에서 막힌다 — 그 하네스가 화면과 마우스를 함께
            // 쥐고 있어서다(2026-09-05 지시).
            //
            //   copy <텍스트>              — 그 글을 그대로
            //   copy --surface %N [줄수]   — 그 pane 의 보이는 화면을 (기본 200줄)
            //   copy --surface %N          — 생략하면 이 pane
            let mut params = json!({});
            if let Some(pos) = args.iter().position(|a| a == "--surface") {
                let surface = args
                    .get(pos + 1)
                    .cloned()
                    .or_else(|| std::env::var("KASATERM_PANE_ID").ok())
                    .ok_or_else(|| anyhow!("copy --surface 뒤에 pane 을 주거나 $KASATERM_PANE_ID 가 있어야 한다"))?;
                params["surface_id"] = json!(surface);
                if let Some(lines) = args.get(pos + 2).and_then(|s| s.parse::<u64>().ok()) {
                    params["lines"] = json!(lines);
                }
            } else {
                let text = args.join(" ");
                if text.trim().is_empty() {
                    return Err(anyhow!(
                        "copy 는 복사할 글이나 --surface <pane> 이 필요하다"
                    ));
                }
                params["text"] = json!(text);
            }
            ("clipboard.set", params)
        }
        "paste" => {
            // 사람이 복사해 둔 것을 캐릭터가 받아 이어서 일할 때. 이름을 `paste` 로
            // 둔 것은 사람의 말에 맞추기 위해서다 — 실제로 하는 일은 읽기다.
            ("clipboard.get", json!({}))
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
            if args.first().is_some_and(|arg|arg == "--address") {
                let address: Value = serde_json::from_str(args.get(1).ok_or_else(||anyhow!("--address requires JSON"))?)
                    .context("invalid activity address JSON")?;
                if !address.is_object() || args.len() > 3 { return Err(anyhow!("activity --address JSON [limit]")); }
                let limit = args.get(2).map(|n|n.parse::<u64>()).transpose().context("invalid activity limit")?.unwrap_or(20).clamp(1,50);
                return Ok(Request{id,method:"collab.inspect".into(),params:json!({"address":address,"limit":limit})});
            }
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

/// tell 본문 머리에 발신 학생 마커 `⟦이름⟧ ` 를 심는다. 이미 마커로 시작하면(재시도·
/// 중계) 두 번 심지 않는다. 이름이 없으면(사람이 친 cli) 본문 그대로.
fn mark_tell_sender(body: String, character: Option<&str>) -> String {
    let Some(name) = character.map(str::trim).filter(|s| !s.is_empty()) else { return body };
    let trimmed = body.trim_start();
    if trimmed.starts_with('⟦') { return body; }
    format!("⟦{name}⟧ {trimmed}")
}

/// `tell` 의 대상이 이름이면 보드에서 주소로 바꾼다 — `이름`·`이름@기계`·`%N@기계`·방 제목.
/// `%N`·`--address` 는 그대로 둔다. 하나만 맞아야 보낸다 — 둘 이상이면 후보를 보여 주고 멈춘다.
/// 이름으로 찾았으면 `이름@기계` 를 돌려준다 — 쪽지가 막혔다는 알림에서 누구에게 보낸 것인지 말한다.
fn resolve_tell_target(args: &mut Vec<String>) -> Result<Option<String>> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--id" | "--title" => i += 2,
            "--stdin" | "--force" => i += 1,
            "--address" | "--" => return Ok(None),
            a if a.starts_with('%') && !a.contains('@') => return Ok(None),
            a if a.starts_with("--") => return Err(anyhow!("모르는 옵션: {a}")),
            _ => break,
        }
    }
    let Some(target) = args.get(i).cloned() else { return Ok(None) };
    let panes = snapshot_rows(&resolve_socket_path()?, false)?;
    match board_matches(&panes, &target).as_slice() {
        [] => Err(anyhow!("「{target}」 이(가) 보드에 없어요 — `kasaterm-cli rooms` 로 이름을 확인하세요")),
        [one] => {
            let address = one.get("address").cloned().unwrap_or(Value::Null);
            eprintln!("→ {}", describe_row(one));
            args.splice(i..i + 1, ["--address".to_string(), address.to_string()]);
            let who = row_text(one, "character");
            Ok(Some(if who.is_empty() { target } else { format!("{who}@{}", row_text(one, "machine_label")) }))
        }
        many => Err(anyhow!("「{target}」 이(가) 여럿이에요 — 이름@기계 로 골라 주세요:\n{}",
            many.iter().map(|p| format!("  {}", describe_row(p))).collect::<Vec<_>>().join("\n"))),
    }
}

/// 이 기계에서 보낸 tell 의 ID → 주소. `tell --status ID` 를 주소 없이 치게 해 준다.
fn receipts_path() -> Option<std::path::PathBuf> {
    Some(crate::home_dir()?.join(".config/kasaterm/tell-receipts.json"))
}

fn load_receipt(id: &str) -> Option<Value> {
    let path = receipts_path()?;
    let map: serde_json::Map<String, Value> = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    map.get(id).cloned()
}

fn save_receipt(id: &str, address: &Value) {
    let Some(path) = receipts_path() else { return };
    let mut map: serde_json::Map<String, Value> = std::fs::read_to_string(&path).ok()
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    map.insert(id.to_string(), address.clone());
    // 영수증은 24시간 뒤 만료된다 — 오래된 것부터 걷어 200개만 둔다(ID 앞이 발행 시각).
    while map.len() > 200 {
        let Some(oldest) = map.keys().min().cloned() else { break };
        map.remove(&oldest);
    }
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let _ = std::fs::write(&path, serde_json::to_string(&Value::Object(map)).unwrap_or_default());
}

/// 오케스트레이터 보고(`done` 이 싣는다) 인자 + env → `nacho.report` 파라미터. env 를 함수로 받는 것은 테스트가
/// 프로세스 env 를 안 건드리고 origin 게이트를 재기 위해서다.
fn orchestrator_report_params(args: &[String], get_env: &dyn Fn(&str) -> Option<String>) -> Result<Value> {
    use crate::nacho_inbox as inbox;
    let origin = inbox::origin_from(get_env).ok_or_else(|| anyhow!(
        "orchestrator report is only for panes an orchestrator started ({} is not set here) — report to whoever gave you the brief instead",
        inbox::ENV_ORIGIN))?;
    let mut params = json!({
        "origin": inbox::ORIGIN,
        "conv": origin.conv,
        "task_id": origin.task_id,
        "machine_id": origin.machine,
        "run_id": origin.run,
        "surface": get_env("KASATERM_PANE_ID").unwrap_or_default(),
        "cwd": std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        "character": get_env("KASATERM_CHARACTER").unwrap_or_default(),
        "harness": if get_env("CLAUDECODE").is_some() { "claude" } else if get_env("CODEX_HOME").is_some() { "codex" } else { "" },
        "changed": [],
    });
    let mut index = 0;
    let mut stdin = false;
    let mut dry_run = false;
    let mut changed: Vec<String> = Vec::new();
    while let Some(arg) = args.get(index) {
        let value = |flag: &str| args.get(index + 1).cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match arg.as_str() {
            "--status" => { params["status"] = json!(value("--status")?); index += 2; }
            "--summary" => { params["summary"] = json!(value("--summary")?); index += 2; }
            "--tests" => { params["tests"] = json!(value("--tests")?); index += 2; }
            "--next" => { params["next"] = json!(value("--next")?); index += 2; }
            "--changed" => { changed.push(value("--changed")?); index += 2; }
            "--conv" => { params["conv"] = json!(value("--conv")?); index += 2; }
            "--task" => { params["task_id"] = json!(value("--task")?); index += 2; }
            "--machine" => { params["machine_id"] = json!(value("--machine")?); index += 2; }
            "--run" => { params["run_id"] = json!(value("--run")?); index += 2; }
            "--stdin" => { stdin = true; index += 1; }
            "--dry-run" => { dry_run = true; index += 1; }
            other => return Err(anyhow!("done: unknown argument {other:?} (flags: --status --summary --changed --tests --next --conv --task --machine --run --stdin --dry-run)")),
        }
    }
    if stdin {
        use std::io::Read;
        let mut text = String::new();
        std::io::stdin().take(inbox::MAX_BYTES as u64 + 1).read_to_string(&mut text)?;
        let extra: Value = serde_json::from_str(&text).context("--stdin expects a JSON object with status/summary/changed/tests/next")?;
        let obj = extra.as_object().ok_or_else(|| anyhow!("--stdin JSON must be an object"))?;
        for (k, v) in obj {
            if matches!(k.as_str(), "origin" | "machine_id" | "conv" | "task_id" | "run_id") && !params[k].as_str().unwrap_or("").is_empty() {
                continue; // env 가 정한 origin 은 stdin 이 못 덮는다
            }
            if k == "changed" { if let Some(items) = v.as_array() { changed.extend(items.iter().filter_map(|x| x.as_str()).map(str::to_string)); } else if let Some(t) = v.as_str() { changed.push(t.to_string()); } continue; }
            params[k] = v.clone();
        }
    }
    params["changed"] = json!(changed.iter().flat_map(|c| c.split(|ch| ch == ',' || ch == '\n')).map(str::trim).filter(|c| !c.is_empty()).collect::<Vec<_>>());
    if params["host"].is_null() {
        let machine_id = get_env("KASATERM_MACHINE_ID").filter(|v| !v.trim().is_empty())
            .or_else(|| crate::home_dir().and_then(|h| std::fs::read_to_string(h.join(".config/kasaterm/machine-id")).ok()).map(|t| t.trim().to_string()))
            .unwrap_or_default();
        let label = get_env("KASATERM_SELF_LABEL").filter(|v| !v.trim().is_empty())
            .or_else(|| std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()))
            .unwrap_or_default();
        params["host"] = json!({"machine_id": machine_id, "label": label});
    }
    if dry_run { params["dry_run"] = json!(true); }
    Ok(params)
}

/// `app-restart plan [--machine ID]… [--json]` · `status JOB [--machine ID]` · `run --approval ap_… [--machine ID]…`.
/// 기기는 명부의 안정 id 로만 고른다 — 이 기기는 소켓으로, 다른 기기는 이 앱이 명부 경유로 묻는다.
/// 승인은 오케스트레이터가 쥔다: 이 CLI 는 키를 모르고, 소비·조회는 이 기기 앱이 오케스트레이터에 대신 한다.
#[cfg(feature = "app-update")]
fn run_app_restart(args: &[String]) -> Result<Option<Response>> {
    use crate::app_restart as restart;
    let sub = args.first().map(String::as_str).unwrap_or("");
    let mut machines: Vec<String> = Vec::new();
    let mut json_out = false;
    let mut approval = String::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--machine" => {
                let value = args.get(i + 1).ok_or_else(|| anyhow!("--machine needs a machine id"))?;
                machines.extend(value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
                i += 2;
            }
            "--approval" => {
                approval = args.get(i + 1).ok_or_else(|| anyhow!("--approval needs an approval id"))?.clone();
                i += 2;
            }
            "--json" => { json_out = true; i += 1; }
            other if other.starts_with("--") => return Err(anyhow!("app-restart: unknown option {other:?}")),
            other => { positional.push(other.to_string()); i += 1; }
        }
    }
    let socket = resolve_socket_path()?;
    let ask = |method: &str, params: Value| -> std::result::Result<Value, String> {
        let request = Request { id: json!(format!("cli-{}", std::process::id())), method: method.into(), params };
        let response = roundtrip(&socket, &request).map_err(|e| e.to_string())?;
        if !response.ok {
            return Err(response.error.map(|e| e.message).unwrap_or_else(|| "request failed".into()));
        }
        Ok(response.result.unwrap_or(Value::Null))
    };
    let facts = |id: &str| -> std::result::Result<restart::Facts, String> {
        let params = if id.is_empty() { json!({}) } else { json!({"machine_id": id}) };
        serde_json::from_value(ask("app.restart_facts", params)?).map_err(|e| format!("사실을 읽지 못했다: {e}"))
    };
    match sub {
        "plan" | "run" => {
            // 조종 기기는 이 앱이 스스로 대는 id 다 — 파일을 따로 읽으면 격리 리그·옛 판에서 어긋난다.
            let local = facts("").map_err(anyhow::Error::msg)?.machine_id;
            anyhow::ensure!(!local.is_empty(), "이 앱이 자기 machine id 를 대지 못했다");
            if machines.is_empty() { machines.push(local.clone()); }
            let plan = restart::build_plan(&local, &machines, &facts, crate::board::now_ms());
            if json_out {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print!("{}", render_restart_plan(&plan));
            }
            if sub == "plan" {
                return Ok(None);
            }
            anyhow::ensure!(!approval.is_empty(), "run 은 오케스트레이터 대화에서 받은 --approval ap_… 가 있어야 한다");
            let transport = CliRestartTransport { ask: &ask, facts: &facts };
            let authority = CliAuthority { ask: &ask };
            let policy = restart::RunPolicy {
                poll_every: std::time::Duration::from_millis(500),
                boot_timeout: std::time::Duration::from_secs(90),
                max_status_errors: 40,
            };
            let outcomes = restart::run(&plan, &approval, &local, &authority, &transport, &policy, &crate::board::now_ms);
            let mut ok = true;
            for (machine, outcome) in &outcomes {
                ok &= matches!(outcome, restart::TargetOutcome::Verified { .. } | restart::TargetOutcome::HandedOff { .. });
                println!("{machine} {}", serde_json::to_string(outcome)?);
            }
            if !ok {
                std::process::exit(1);
            }
            Ok(None)
        }
        "status" => {
            let job = positional.first().ok_or_else(|| anyhow!("app-restart status needs a job id"))?;
            let value = ask("app.restart_job", json!({"job_id": job, "machine_id": machines.first()})).map_err(anyhow::Error::msg)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(None)
        }
        _ => Err(anyhow!("app-restart plan [--machine ID]… [--json] | run --approval ap_… [--machine ID]… | status JOB [--machine ID]")),
    }
}

/// `app-update start --machine ID --request FILE|-` · `status JOB [--machine ID]` ·
/// `run --approval ap_… --rollout FILE [--record FILE]` (조종 쪽 러너 — `crate::app_update::run`).
/// 요청(`crate::app_update::UpdateRequest`)은 조종 쪽이 계획·오케스트레이터 승인으로 만든다. 이 CLI 는 모양만 보고 넘기며,
/// 받을지·갈아 끼울지는 대상 기기 앱이 자기 사실과 오케스트레이터 승인으로 다시 판정한다.
#[cfg(feature = "app-update")]
fn run_app_update(args: &[String]) -> Result<Option<Response>> {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let (mut machine, mut request, mut positional) = (None::<String>, None::<String>, Vec::new());
    let (mut approval, mut rollout_path, mut record_path) = (String::new(), None::<String>, None::<String>);
    let mut i = 1;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "--machine" => { machine = Some(args.get(i + 1).ok_or_else(|| anyhow!("--machine needs a machine id"))?.clone()); i += 2; }
            "--request" => { request = Some(args.get(i + 1).ok_or_else(|| anyhow!("--request needs a file or -"))?.clone()); i += 2; }
            "--approval" => { approval = args.get(i + 1).ok_or_else(|| anyhow!("--approval needs an approval id"))?.clone(); i += 2; }
            "--rollout" => { rollout_path = Some(args.get(i + 1).ok_or_else(|| anyhow!("--rollout needs a file"))?.clone()); i += 2; }
            "--record" => { record_path = Some(args.get(i + 1).ok_or_else(|| anyhow!("--record needs a file"))?.clone()); i += 2; }
            other if other.starts_with("--") => return Err(anyhow!("app-update: unknown option {other:?}")),
            other => { positional.push(other.to_string()); i += 1; }
        }
    }
    let socket = resolve_socket_path()?;
    let ask = |method: &str, params: Value| -> Result<Value> {
        let req = Request { id: json!(format!("cli-{}", std::process::id())), method: method.into(), params };
        let response = roundtrip(&socket, &req)?;
        anyhow::ensure!(response.ok, "{}", response.error.map(|e| e.message).unwrap_or_else(|| "request failed".into()));
        Ok(response.result.unwrap_or(Value::Null))
    };
    match sub {
        "start" => {
            let machine = machine.ok_or_else(|| anyhow!("app-update start needs --machine ID"))?;
            let text = match request.as_deref() {
                Some("-") => { let mut s = String::new(); std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?; s }
                Some(path) => std::fs::read_to_string(path)?,
                None => return Err(anyhow!("app-update start needs --request FILE|-")),
            };
            let parsed: crate::app_update::UpdateRequest = serde_json::from_str(&text).map_err(|e| anyhow!("요청을 읽지 못했다: {e}"))?;
            crate::app_update::check_job(&parsed.job).map_err(|e| anyhow!("요청 모양이 아니다: {e}"))?;
            let value = ask("app.update_start", json!({"machine_id": machine, "request": parsed}))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(None)
        }
        "status" => {
            let job = positional.first().ok_or_else(|| anyhow!("app-update status needs a job id"))?;
            let value = ask("app.update_job", json!({"job_id": job, "machine_id": machine}))?;
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(None)
        }
        "run" => {
            use crate::app_update as update;
            let path = rollout_path.ok_or_else(|| anyhow!("app-update run needs --rollout FILE"))?;
            anyhow::ensure!(!approval.is_empty(), "app-update run 은 오케스트레이터 대화에서 받은 --approval ap_… 가 있어야 한다");
            let rollout: update::Rollout = serde_json::from_str(&std::fs::read_to_string(&path)?).map_err(|e| anyhow!("rollout 을 읽지 못했다: {e}"))?;
            let ask_s = |method: &str, params: Value| ask(method, params).map_err(|e| e.to_string());
            let local: crate::app_restart::Facts = serde_json::from_value(ask_s("app.restart_facts", json!({})).map_err(anyhow::Error::msg)?)?;
            // 러너는 조종 기기에 서 있어야 한다 — 승인을 소비하는 기기이고, 자기를 마지막에 넘긴다.
            anyhow::ensure!(local.machine_id == rollout.controller(), "이 기기({})는 rollout 의 조종 기기({})가 아니다", local.machine_id, rollout.controller());
            let record = record_path.clone();
            let grant: Option<update::Grant> = match &record {
                Some(p) if std::path::Path::new(p).exists() => Some(serde_json::from_str(&std::fs::read_to_string(p)?)?),
                _ => None,
            };
            let save = |g: &update::Grant| -> std::result::Result<(), String> {
                let Some(p) = &record else { return Ok(()) };
                let tmp = format!("{p}.tmp");
                std::fs::write(&tmp, serde_json::to_string_pretty(g).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, p).map_err(|e| e.to_string())
            };
            let transport = CliUpdateTransport { ask: &ask_s };
            let authority = CliAuthority { ask: &ask_s };
            let outcomes = update::run(&rollout, &approval, &authority, &transport, &update::RunPolicy::default(), grant.as_ref(), &save,
                                       &crate::board::now_ms, &std::thread::sleep);
            let mut ok = true;
            for (machine, outcome) in &outcomes {
                ok &= matches!(outcome, update::Outcome::Updated { .. } | update::Outcome::HandedOff { .. });
                println!("{machine} {}", serde_json::to_string(outcome)?);
            }
            if !ok {
                std::process::exit(1);
            }
            Ok(None)
        }
        _ => Err(anyhow!("app-update start --machine ID --request FILE|- | status JOB [--machine ID] | run --approval ap_… --rollout FILE [--record FILE]")),
    }
}

#[cfg(feature = "app-update")]
type Ask<'a> = &'a dyn Fn(&str, Value) -> std::result::Result<Value, String>;

/// 업데이트 대상에 닿는 길 — 이 기기 앱의 소켓을 거친다(다른 기기는 앱이 명부 경유 HTTP 로 넘긴다).
#[cfg(feature = "app-update")]
struct CliUpdateTransport<'a> {
    ask: Ask<'a>,
}

#[cfg(feature = "app-update")]
impl crate::app_update::Transport for CliUpdateTransport<'_> {
    fn facts(&self, machine_id: &str) -> std::result::Result<crate::app_restart::Facts, String> {
        serde_json::from_value((self.ask)("app.restart_facts", json!({"machine_id": machine_id}))?).map_err(|e| e.to_string())
    }
    fn start(&self, machine_id: &str, req: &crate::app_update::UpdateRequest) -> std::result::Result<Value, crate::app_update::Reach> {
        (self.ask)("app.update_start", json!({"machine_id": machine_id, "request": req})).map_err(|e| crate::app_update::reach_of(&e))
    }
    fn status(&self, machine_id: &str, job_id: &str) -> std::result::Result<crate::app_update::Status, String> {
        serde_json::from_value((self.ask)("app.update_job", json!({"job_id": job_id, "machine_id": machine_id}))?).map_err(|e| e.to_string())
    }
}

/// 대상 기기에 닿는 길 — 전부 이 기기 앱의 소켓을 거친다(다른 기기는 앱이 명부 경유로 넘긴다).
#[cfg(feature = "app-update")]
struct CliRestartTransport<'a> {
    ask: Ask<'a>,
    facts: &'a dyn Fn(&str) -> std::result::Result<crate::app_restart::Facts, String>,
}

#[cfg(feature = "app-update")]
impl crate::app_restart::Transport for CliRestartTransport<'_> {
    fn facts(&self, machine_id: &str) -> std::result::Result<crate::app_restart::Facts, String> {
        (self.facts)(machine_id)
    }
    fn start(&self, machine_id: &str, req: &crate::app_restart::JobRequest) -> std::result::Result<(), String> {
        (self.ask)("app.restart_start", json!({"machine_id": machine_id, "request": req})).map(|_| ())
    }
    fn status(&self, machine_id: &str, job_id: &str) -> std::result::Result<crate::app_restart::JobState, String> {
        let value = (self.ask)("app.restart_job", json!({"job_id": job_id, "machine_id": machine_id}))?;
        serde_json::from_value(value["state"].clone()).map_err(|e| e.to_string())
    }
}

/// 오케스트레이터 승인 — 소비·조회는 이 기기 앱이 오케스트레이터 앱 창구에 대신 한다(키는 앱 밖으로 안 나온다).
#[cfg(feature = "app-update")]
struct CliAuthority<'a> {
    ask: Ask<'a>,
}

#[cfg(feature = "app-update")]
impl crate::app_restart::Authority for CliAuthority<'_> {
    fn get(&self, approval_id: &str) -> std::result::Result<crate::app_restart::ApprovalView, String> {
        serde_json::from_value((self.ask)("app.restart_approval", json!({"approval_id": approval_id}))?).map_err(|e| e.to_string())
    }
    fn consume(&self, approval_id: &str, scope: &Value, consumer: &str) -> std::result::Result<crate::app_restart::ApprovalView, String> {
        let value = (self.ask)("app.restart_consume", json!({"approval_id": approval_id, "scope": scope, "consumer_machine_id": consumer}))?;
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
}

#[cfg(feature = "app-update")]
fn render_restart_plan(plan: &crate::app_restart::Plan) -> String {
    let mut out = format!("재시작 계획 {} · {}분 유효 · 순서대로 한 대씩(조종 기기는 마지막)\n", plan.hash, crate::app_restart::PLAN_TTL_MS / 60_000);
    for (n, target) in plan.targets.iter().enumerate() {
        let name = if target.label.is_empty() { target.machine_id.clone() } else { format!("{} ({})", target.label, target.machine_id) };
        out.push_str(&format!("{}. {}{}\n", n + 1, name, if target.controller { " · 조종 기기" } else { "" }));
        if let Some(f) = &target.facts {
            out.push_str(&format!(
                "   pid {} · 빌드 {} · 이어지는 학생 대화 {} · 새 셸로만 오는 창 {} · 등록 서버 {} · 펫 {}\n",
                f.pid, f.binary.build, f.restorable_sessions, f.plain_shells, f.registered_servers,
                match f.pet_alive { Some(true) => "켜짐", Some(false) => "꺼짐", None => "모름" },
            ));
        }
        if target.refusals.is_empty() {
            out.push_str("   재시작 가능\n");
        }
        for refusal in &target.refusals {
            out.push_str(&format!("   거부 · {}\n", refusal.message()));
        }
    }
    out.push_str(if plan.runnable() { "모든 기기 가능 — 실행은 오케스트레이터 대화의 주인 확인 단추로 받은 승인(--approval)이 있어야 한다\n" } else { "거부 사유가 있어 실행할 수 없다\n" });
    out
}

/// 이 창의 판 주소 UUID. 창 번호(`%N`)는 재사용되므로 오케스트레이터는 이것으로 등록 줄을 찾는다.
/// 이 기계의 판에서 번호로 찾고, 못 찾으면 빈 값 — 보고는 번호만으로도 간다.
fn local_surface_key(surface: &str) -> String {
    if surface.is_empty() || API_TARGET.get().is_some() {
        return String::new();
    }
    let Ok(socket_path) = resolve_socket_path() else { return String::new() };
    let request = Request { id: json!(format!("cli-{}", std::process::id())), method: "collab.snapshot".into(), params: json!({"scope": "local"}) };
    let Ok(response) = roundtrip(&socket_path, &request) else { return String::new() };
    surface_key_in(response.result.as_ref(), surface)
}

fn surface_key_in(snapshot: Option<&Value>, surface: &str) -> String {
    let panes = snapshot.and_then(|r| r.get("panes")).and_then(|p| p.as_array());
    let keys: Vec<&str> = panes.into_iter().flatten()
        .filter_map(|p| p.get("address"))
        .filter(|a| a.get("surface_id").and_then(|v| v.as_str()) == Some(surface))
        .filter_map(|a| a.get("surface_key").and_then(|v| v.as_str()))
        .collect();
    // 같은 번호가 둘이면(거울 창 등) 어느 쪽인지 모른다 — 고르지 않고 비운다.
    match keys.as_slice() { [one] => one.to_string(), _ => String::new() }
}

fn run_orchestrator_report(args: &[String]) -> Result<Option<Response>> {
    use crate::nacho_inbox as inbox;
    let get_env = |k: &str| std::env::var(k).ok();
    let mut params = orchestrator_report_params(args, &get_env)?;
    let key = local_surface_key(params["surface"].as_str().unwrap_or(""));
    if !key.is_empty() {
        params["surface_key"] = json!(key);
    }
    let dry_run = params["dry_run"] == true;
    params.as_object_mut().map(|o| o.remove("dry_run"));
    // build 를 여기서 한 번 돌려 오류를 학생에게 바로 보인다(원격이면 저쪽이 또 검증한다).
    let envelope = inbox::build(&params)?;
    if dry_run {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(None);
    }
    let target = params["machine_id"].as_str().unwrap_or("").trim().to_string();
    let local_id = params["host"]["machine_id"].as_str().unwrap_or("").trim().to_string();
    let local_label = params["host"]["label"].as_str().unwrap_or("").trim().to_string();
    let is_local = target.is_empty() || target == local_id || (!local_label.is_empty() && target == local_label);
    let receipt = if is_local && API_TARGET.get().is_none() {
        inbox::deposit(&envelope)?
    } else {
        let request = Request { id: json!(format!("cli-{}", std::process::id())), method: "nacho.report".into(), params };
        let response = roundtrip(&resolve_socket_path()?, &request)?;
        if !response.ok {
            return Ok(Some(response));
        }
        response.result.unwrap_or(json!({}))
    };
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        let wake = match receipt["wake"].as_str() {
            Some("socket") => "오케스트레이터가 지금 깼다".to_string(),
            Some("queued") => format!("오케스트레이터가 안 듣는다 — 파일은 남았고 다음 부팅·폴링에서 집는다 ({})", receipt["wake_note"].as_str().unwrap_or("")),
            _ => "앞선 같은 보고가 이미 깨웠다".to_string(),
        };
        println!("오케스트레이터 인박스 {} · {} · {}", receipt["state"].as_str().unwrap_or("?"), wake, receipt["report_id"].as_str().unwrap_or(""));
    } else {
        println!("{}", serde_json::to_string(&receipt)?);
    }
    Ok(None)
}

const OP_USAGE: &str = "op read <op://금고/항목/필드> | op run -e 이름=<op://…> [-e …] -- <명령…> | op status";

/// 앱에 비밀 읽기를 맡기고 폰 허락을 기다린다. 값은 참조 순서대로.
fn op_values(refs: &[String]) -> Result<Vec<String>> {
    let socket_path = resolve_socket_path()?;
    eprintln!("kasaterm: 폰에서 Face ID 로 허락하면 읽어요(2분 안) — {}", refs.join(", "));
    let req = Request { id: json!("op"), method: "op.secret".into(), params: json!({ "op": "read", "refs": refs }) };
    let resp = roundtrip(&socket_path, &req)?;
    if !resp.ok {
        let message = resp.error.as_ref().map(|e| e.message.clone()).unwrap_or_default();
        return Err(anyhow!("{}", op_reason(&message)));
    }
    let values: Vec<String> = resp.result.as_ref().and_then(|r| r["values"].as_array()).into_iter().flatten()
        .filter_map(|v| v.as_str().map(str::to_string)).collect();
    if values.len() != refs.len() {
        return Err(anyhow!("앱이 값을 다 돌려주지 않았어요"));
    }
    Ok(values)
}

fn op_reason(code: &str) -> String {
    let head = code.split(':').next().unwrap_or(code).trim();
    let say = match head {
        "op_token_missing" => "이 맥에 1Password 토큰이 없어요 — 사람이 설정 → 계정 → 1Password 에서 넣어야 해요",
        "no_trusted_key" => "이 맥이 믿는 폰 Face ID 열쇠가 없어요 — 폰 설정에서 열쇠를 만들고 맥 설정에서 믿기를 눌러야 해요",
        "vault_not_allowed" => "허용된 금고 밖의 참조예요",
        "bad_ref" => "참조 모양이 틀렸어요(op://금고/항목/필드)",
        "denied" => "폰에서 거절했어요",
        "expired" => "2분 안에 허락이 오지 않았어요",
        "cancelled" => "요청이 취소됐어요",
        "not_in_pane" | "requester_unknown" => "kasaterm 칸 안에서만 쓸 수 있어요",
        "signed_out" | "isolated_run" => "이 기기가 KASA 계정에 로그인돼 있지 않아요",
        "helper_missing" => "이 앱 판에 1Password 실행기(kasa-op)가 없어요",
        "helper_unverified" => "1Password 실행기의 서명을 확인하지 못했어요",
        "update_required" => "관문이 이 기능을 아직 몰라요",
        _ => "",
    };
    if say.is_empty() { code.to_string() } else { format!("{say} ({head})") }
}

fn run_op(args: &[String]) -> Result<Option<Response>> {
    match args.first().map(String::as_str) {
        Some("status") => {
            let socket_path = resolve_socket_path()?;
            let req = Request { id: json!("op"), method: "op.secret".into(), params: json!({ "op": "status" }) };
            let resp = roundtrip(&socket_path, &req)?;
            let v = resp.result.unwrap_or_default();
            println!("토큰: {}", if v["token"] == true { format!("있음 · 금고 {}", v["vault"].as_str().unwrap_or("")) } else { "없음".into() });
            let keys: Vec<String> = v["trusted"].as_array().into_iter().flatten()
                .map(|k| format!("{} {}", k["label"].as_str().unwrap_or(""), k["fingerprint"].as_str().unwrap_or(""))).collect();
            println!("믿는 폰 열쇠: {}", if keys.is_empty() { "없음".into() } else { keys.join(", ") });
            println!("실행기: {}", if v["helper"] == true { "있음" } else { "없음" });
            Ok(None)
        }
        Some("read") => {
            let reference = args.iter().skip(1).find(|a| !a.starts_with('-')).ok_or_else(|| anyhow!("{OP_USAGE}"))?;
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) && !args.iter().any(|a| a == "--reveal") {
                return Err(anyhow!("값이 화면에 그대로 찍혀요 — `x=$(kasaterm-cli op read …)` 로 담거나 `op run -e` 를 쓰세요(정말 보려면 --reveal)"));
            }
            let value = op_values(std::slice::from_ref(reference))?.remove(0);
            let mut out = std::io::stdout();
            out.write_all(value.as_bytes())?;
            out.flush()?;
            Ok(None)
        }
        Some("run") => {
            let mut names = Vec::new();
            let mut refs = Vec::new();
            let mut i = 1;
            while i < args.len() && args[i] != "--" {
                let pair = match args[i].as_str() {
                    "-e" | "--env" => { i += 1; args.get(i).cloned().unwrap_or_default() }
                    other => other.strip_prefix("--env=").map(str::to_string).ok_or_else(|| anyhow!("{OP_USAGE}"))?,
                };
                let (name, reference) = pair.split_once('=').ok_or_else(|| anyhow!("-e 이름=op://… 모양이어야 해요"))?;
                let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !valid || !reference.starts_with("op://") {
                    return Err(anyhow!("-e 이름=op://… 모양이어야 해요: {pair}"));
                }
                names.push(name.to_string());
                refs.push(reference.to_string());
                i += 1;
            }
            let command = args.get(i + 1..).filter(|c| !c.is_empty()).ok_or_else(|| anyhow!("{OP_USAGE}"))?;
            if refs.is_empty() {
                return Err(anyhow!("{OP_USAGE}"));
            }
            let values = op_values(&refs)?;
            let code = op_run_child(command, &names, &values)?;
            std::process::exit(code);
        }
        _ => Err(anyhow!("{OP_USAGE}")),
    }
}

/// 값을 환경으로만 넘겨 명령을 돌리고, 그 출력에 값이 나오면 가린다(op run 과 같은 생각).
fn op_run_child(command: &[String], names: &[String], values: &[String]) -> Result<i32> {
    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .envs(names.iter().zip(values))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("실행 못 함: {}", command[0]))?;
    let mut hidden: Vec<String> = Vec::new();
    for value in values {
        hidden.push(value.clone());
        hidden.extend(value.lines().filter(|l| l.trim().len() >= 6).map(str::to_string));
    }
    hidden.retain(|h| h.len() >= 4);
    hidden.sort_by_key(|h| std::cmp::Reverse(h.len()));
    let pump = |source: Box<dyn std::io::Read + Send>, err: bool, hidden: Vec<String>| {
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(source);
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line).map(|n| n > 0).unwrap_or(false) {
                let mut text = String::from_utf8_lossy(&line).into_owned();
                for h in &hidden {
                    text = text.replace(h.as_str(), "<concealed by kasaterm>");
                }
                if err { let _ = std::io::stderr().write_all(text.as_bytes()); } else { let _ = std::io::stdout().write_all(text.as_bytes()); }
                line.clear();
            }
        })
    };
    let out = pump(Box::new(child.stdout.take().unwrap()), false, hidden.clone());
    let err = pump(Box::new(child.stderr.take().unwrap()), true, hidden);
    let status = child.wait()?;
    let _ = out.join();
    let _ = err.join();
    let _ = std::io::stdout().flush();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Ok(128 + signal);
        }
    }
    Ok(status.code().unwrap_or(1))
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
    if let Some(target) = API_TARGET.get() { return api_roundtrip(target,request); }
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
    let resp: Response = serde_json::from_str(line.trim()).context("parse response JSON")?;
    Ok(resp)
}

fn api_roundtrip(target: &ApiTarget, request: &Request) -> Result<Response> {
    use std::io::Read;
    use std::process::{Command,Stdio};
    let (path,post) = match request.method.as_str() {
        "collab.snapshot" => ("/collab/board",false),
        "collab.changes" => ("/collab/changes",false),
        "collab.inspect" => ("/collab/inspect",false),
        "collab.tell" => ("/collab/tell",true),
        "collab.tell_status" => ("/collab/tell/status",true),
        "nacho.report" => ("/nacho/report",true),
        _ => return Err(anyhow!("this command has no safe HTTP mapping; use board --all or activity --address")),
    };
    let quote = |text: &str| format!("\"{}\"",text.replace('\\',"\\\\").replace('"',"\\\"").replace('\n',"\\n").replace('\r',"\\r"));
    // Credentials and message bodies travel through stdin, never process arguments.
    let mut config = format!("silent\nshow-error\nfail-with-body\nmax-time = 5\nconnect-timeout = 3\nmax-filesize = 4194304\nproto = \"=http,https\"\nmax-redirs = 0\nurl = {}\n",quote(&format!("{}{path}",target.base)));
    if let Some(path) = &target.token_file {
        let mut token = String::new();
        std::fs::File::open(path).context("open API token file")?.take(4097).read_to_string(&mut token)?;
        let token = token.trim();
        if token.is_empty() || token.len() > 4096 || token.chars().any(char::is_control) { return Err(anyhow!("invalid API token file")); }
        config.push_str(&format!("header = {}\n",quote(&format!("x-kasa-token: {token}"))));
    }
    if post {
        config.push_str(&format!("request = POST\nheader = \"Content-Type: application/json\"\ndata = {}\n",quote(&request.params.to_string())));
    } else {
        config.push_str(&format!("get\ndata-urlencode = {}\n",quote(&format!("params={}",request.params))));
    }
    let mut child = Command::new("curl").args(["--disable","--config","-"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("start HTTP transport (curl)")?;
    child.stdin.take().context("HTTP transport stdin missing")?.write_all(config.as_bytes())?;
    let mut bytes = Vec::new();
    child.stdout.take().context("HTTP transport stdout missing")?.take(4*1024*1024+1).read_to_end(&mut bytes)?;
    if bytes.len() > 4*1024*1024 { let _ = child.kill(); let _ = child.wait(); return Err(anyhow!("HTTP response exceeds limit")); }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let reason = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v|v["error"].as_str().map(str::to_owned))
            .unwrap_or_else(||"HTTP request failed; check the explicit API address and authentication".into());
        return Ok(Response::error(request.id.clone(),crate::protocol::codes::BACKEND_ERROR,reason));
    }
    let value: Value = serde_json::from_slice(&bytes).context("invalid HTTP response JSON")?;
    if value["ok"] == false {
        return Ok(Response::error(request.id.clone(),crate::protocol::codes::BACKEND_ERROR,
            value["error"].as_str().unwrap_or("HTTP collaboration request failed")));
    }
    Ok(Response::success(request.id.clone(),value))
}

// ---------------------------------------------------------------- sessions --

/// 터미널 세션 목록. 목록은 jsonl·SQLite 직스캔이라 claude
/// /resume 의 teamName 필터를 안 탄다.
///
/// 기본은 **세 하네스 전체**(claude·codex·agy)를 가로지른다 — 하네스가 셋이 된
/// 뒤로 "어디서 뭘 하다 말았나"를 한 자리에서 봐야 하기 때문이다. `--here` 는
/// 예전처럼 지금 cwd 의 claude 세션만 본다(그 프로젝트 것만 훑을 때).
fn run_sessions_picker(args: &[String]) -> Result<()> {
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
        Some("claude") if here => crate::sessions::recent_sessions_for(&cwd, limit),
        Some("claude") => crate::sessions::recent_claude_sessions_all(limit),
        Some("codex") if here => crate::sessions::recent_codex_sessions_for(&cwd, limit),
        Some("codex") => crate::sessions::recent_codex_sessions(limit),
        Some("agy") => crate::sessions::recent_agy_sessions(limit),
        // 하네스를 안 고른 `--here` 는 세 하네스를 가로지른다 — 예전엔 claude 만
        // 봐서, 이 폴더에서 codex 로 일한 기록이 목록에 없는 것이 됐다.
        _ if here => crate::sessions::recent_sessions_here(&cwd, limit),
        _ => crate::sessions::recent_all_sessions(limit),
    };
    if list.is_empty() {
        if here {
            println!("최근 세션 없음 ({})", cwd.display());
        } else {
            println!("최근 세션 없음");
        }
        return Ok(());
    }
    let home = crate::home_dir().unwrap_or_default();
    let config = crate::isolated_collab_root().unwrap_or_else(|| home.join(".config/kasaterm"));
    let bindings = read_string_map(&crate::session_storage::read_path(&config, "session_characters.json"));
    let colors = student_colors(&home.join(".config/kasaterm/characters.json"));
    let live = live_session_ids();
    const RESET: &str = "\x1b[0m";
    const DIM: &str = "\x1b[2m";
    for (i, s) in list.iter().enumerate() {
        let student = bindings.get(&s.id).cloned().unwrap_or_default();
        let color = colors.get(&student).map(|h| ansi_fg(h)).unwrap_or_default();
        let dot = if student.is_empty() {
            format!("{DIM}·{RESET}")
        } else {
            format!("{color}●{RESET}")
        };
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
            println!(
                "     {DIM}{}{RESET}",
                clip_display(&s.preview, term_cols().saturating_sub(6))
            );
        }
    }
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
    let home = crate::home_dir().unwrap_or_default();
    let dir = home.join(".claude/sessions");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in entries.flatten() {
        let Some(v) = std::fs::read_to_string(e.path())
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        else {
            continue;
        };
        let Some(sid) = v.get("sessionId").and_then(|s| s.as_str()) else {
            continue;
        };
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
        .map(|c| {
            if ('\u{1100}'..='\u{FFDC}').contains(&c) {
                2
            } else {
                1
            }
        })
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
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 20
        {
            return ws.ws_col as usize;
        }
    }
    std::env::var("COLUMNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 20)
        .unwrap_or(80)
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
        let cw = if ('\u{1100}'..='\u{FFDC}').contains(&c) {
            2
        } else {
            1
        };
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
// pane claude 의 상태줄. 렌더러(screenread)가 이 줄의 표식을 읽어 오버레이를 얹으므로
// 세그먼트 순서·표식을 바꾸면 그쪽 판독도 같이 봐야 한다.

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
// **지우지 마라**: 이 문자가 화면에 있느냐가 세 판정의 근거다 — agents 목록 뷰인지
// (`has_profile_slot`), 상태줄이 stale 이라 재실행해야 하는지(socket.rs), 입력박스 위
// 전신 학생을 어느 행에 세울지(standing). 렌더러가 이 칸을 걷어 화면엔 안 남는다.
const SL_SPRITE: &str = "\u{fffc}";
// kasaterm 안에서 Nerd Font 글리프 대신 찍는 한 칸 표식 — 렌더러가 찾아 지우고 공식 로고를 얹는다.
const SL_MODEL_MARKER_CLAUDE: &str = "\u{e0c0}";
const SL_MODEL_MARKER_GPT: &str = "\u{e0c1}";

struct SlIcons {
    model: &'static str,
    git: &'static str,
    folder: &'static str,
    effort: &'static str,
}

fn sl_icons(set: &str) -> SlIcons {
    match set {
        "unicode" => SlIcons {
            model: ">",
            git: "⎇",
            folder: "▸",
            effort: "↯",
        },
        "plain" => SlIcons {
            model: "M",
            git: "git",
            folder: "dir",
            effort: "E",
        },
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
    crate::home_dir().unwrap_or_default()
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
    let mut pct = ctx
        .get("used_percentage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut win = ctx
        .get("context_window_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let tot = ctx
        .get("total_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mid_owned = d
        .get("model")
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mid = mid_owned.split('[').next().unwrap_or("");
    let known: u64 = if mid == "claude-fable-5" {
        1_000_000
    } else {
        0
    };
    if known > win {
        win = known;
        pct = (tot as f64 / win as f64 * 100.0).min(100.0);
    }
    (win, pct, tot)
}

/// `git rev-parse --abbrev-ref HEAD` 와 같은 답을 HEAD 파일에서 바로 읽는다. 상태줄은 매초 다시
/// 그려져서 pane 마다 git 을 띄우면 그것만으로 한 번에 10ms 씩 든다. 워크트리·서브모듈의 `.git`
/// 파일(`gitdir: …`)도 따라간다.
fn sl_git_branch(cwd: &str) -> Option<String> {
    let mut dir = Path::new(cwd);
    let git_dir = loop {
        let dot = dir.join(".git");
        if dot.is_dir() {
            break dot;
        }
        if let Ok(link) = std::fs::read_to_string(&dot) {
            break dir.join(link.strip_prefix("gitdir:")?.trim());
        }
        dir = dir.parent()?;
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    Some(match head.trim().strip_prefix("ref:") {
        Some(name) => {
            let name = name.trim();
            name.strip_prefix("refs/heads/").unwrap_or(name).to_string()
        }
        None => "HEAD".to_string(),
    })
}

/// 상태줄 한 줄을 짓는 데 필요한 바깥 사실. 환경·파일 읽기를 여기로 모아 줄 짓기는 입력만으로
/// 정해지게 한다.
struct SlSurroundings<'a> {
    in_pane: bool,
    character: Option<&'a str>,
    config: &'a Value,
    settings: &'a Value,
    branch: Option<&'a str>,
    /// 칸 안 mod 가 앱에 알린 비용·한도·백그라운드 수(`<pane>-mod.json`). mod 없는 칸은 None.
    module: Option<&'a Value>,
}

fn sl_window_label(win: u64) -> String {
    if win >= 1_000_000 {
        "1M".to_string()
    } else if win > 0 {
        format!("{}k", win / 1000)
    } else {
        String::new()
    }
}

/// 보드에 뜨는 모델 표시명 — 상태줄에 찍는 것과 같은 글자(창 크기 꼬리 포함).
fn sl_model_label(d: &Value, win: u64) -> String {
    let name = d.pointer("/model/display_name").and_then(Value::as_str).unwrap_or("");
    let name = name.split(" (").next().unwrap_or(name);
    if name.is_empty() {
        return String::new();
    }
    format!("{name} {}", sl_window_label(win)).trim().to_string()
}

fn sl_line(d: &Value, cwd: &str, at: &SlSurroundings) -> String {
    let ic = sl_icons(at.config.get("icon_set").and_then(Value::as_str).unwrap_or("nerd-font"));
    let sep_char = at.config.get("separator").and_then(Value::as_str).unwrap_or("┃");
    let field_on = |field: &str| {
        at.settings
            .get(format!("agent_statusline_{field}"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    };
    let sep = format!(" {SL_DIM}{}{sep_char}{SL_RESET} ", ansi_fg(SL_C_SEP));
    let mut parts: Vec<String> = Vec::new();

    // pane 안에서는 학생을 안 쓴다 — pane 헤더가 이미 보여준다. 표식 한 칸만 왼쪽 끝에 붙이고
    // 구분자는 안 단다(넣으면 `￼ ┃ ` 로 네 칸이 빈다). 밖에서는 헤더가 없으니 여기서만 알 수 있다.
    let mut prefix = "";
    if let Some(name) = at.character {
        if at.in_pane {
            prefix = SL_SPRITE;
        } else {
            let hex = SL_STUDENT_HEX
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| *v)
                .unwrap_or(SL_C_FALLBACK);
            let c = ansi_fg(hex);
            parts.push(format!("{c}●{SL_RESET} {c}{SL_BOLD}{name}{SL_RESET}"));
        }
    }

    let (win, pct, _) = sl_context(d);
    if let Some(model) = d
        .pointer("/model/display_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && field_on("model"))
    {
        let model = model.split(" (").next().unwrap_or(model);
        // 창 크기는 모델의 성질이라 모델 옆에 붙인다 — 같은 Opus 라도 분모가 다섯 배 갈린다.
        let win_s = sl_window_label(win);
        let tail = if win_s.is_empty() {
            String::new()
        } else {
            format!("{SL_DIM} {win_s}{SL_RESET}")
        };
        // kasaterm 안에서는 PUA 표식 한 칸을 찍고 렌더러가 그 자리에 공식 로고를 얹는다.
        let id = d.pointer("/model/id").and_then(Value::as_str).unwrap_or("").to_lowercase();
        let icon = if !at.in_pane {
            ic.model
        } else if id.starts_with("gpt-")
            || id.starts_with("codex-")
            || (id.starts_with('o') && id[1..].starts_with(|c: char| c.is_ascii_digit()))
        {
            SL_MODEL_MARKER_GPT
        } else if id.starts_with("claude-") {
            SL_MODEL_MARKER_CLAUDE
        } else {
            ic.model
        };
        parts.push(format!("{}{SL_BOLD}{icon} {model}{SL_RESET}{tail}", ansi_fg(SL_C_MODEL)));
    }

    if let Some(branch) = at.branch.filter(|s| !s.is_empty()) {
        parts.push(format!("{}{} {branch}{SL_RESET}", ansi_fg(SL_C_GIT), ic.git));
    }

    if field_on("cwd") {
        let dir_name = Path::new(cwd)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        parts.push(format!("{}{} {dir_name}{SL_RESET}", ansi_fg(SL_C_DIR), ic.folder));
    }

    if field_on("usage") {
        let c_ctx = ansi_fg(if pct >= 90.0 { "f7768e" } else { SL_C_CTX });
        parts.push(format!("{c_ctx}{pct:.0}%{SL_RESET}"));
    }

    if let Some(m) = at.module.filter(|_| field_on("usage")) {
        parts.extend(sl_module_parts(m));
    }

    if let Some(lvl) = d
        .pointer("/effort/level")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let hex = SL_EFFORT_HEX
            .iter()
            .find(|(k, _)| *k == lvl)
            .map(|(_, v)| *v)
            .unwrap_or("7aa2f7");
        parts.push(format!("{}{} {lvl}{SL_RESET}", ansi_fg(hex), ic.effort));
    }

    format!("{prefix}{}", parts.join(&sep))
}

#[cfg(test)]
fn strip_ansi_for_test(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// mod 칸의 덧붙임 — 비용만. 한도·백그라운드 수는 상태줄을 붐비게 해 걷었다(2026-10-06).
fn sl_module_parts(m: &Value) -> Vec<String> {
    m.get("cost_usd")
        .and_then(Value::as_f64)
        .filter(|usd| *usd >= 0.01)
        .map(|usd| format!("{SL_DIM}${usd:.2}{SL_RESET}"))
        .into_iter()
        .collect()
}

/// 앱이 이 칸에 쓴 mod 사실 — 같은 세션 것만(다른 세션이 남긴 낡은 파일을 안 읽게).
fn sl_module_facts(pane: &str, session_id: &str) -> Option<Value> {
    let path = std::env::temp_dir().join("kasaterm-statusline").join(format!("{}-mod.json", pane.trim_start_matches('%')));
    let facts = sl_read_json(&path)?;
    (facts.get("session").and_then(Value::as_str) == Some(session_id)).then_some(facts)
}

/// 상태줄은 매초 다시 그려지지만 앱에 알릴 값은 거의 안 바뀐다. 보고 하나가 GUI 이벤트 둘을
/// 일으키므로 같은 값이면 30초에 한 번만 보낸다 — 앱이 다시 켜져 기억을 잃어도 그 안에 되찬다.
/// 열쇠에 부모(claude) 프로세스를 넣어 새로 뜬 claude 의 첫 보고는 막지 않는다.
fn sl_report_due(pane: &str, payload: &str) -> bool {
    #[cfg(unix)]
    let owner = std::os::unix::process::parent_id().to_string();
    #[cfg(not(unix))]
    let owner = String::from("0");
    let dir = std::env::temp_dir().join("kasaterm-statusline");
    let path = dir.join(format!("{}-{owner}", pane.trim_start_matches('%')));
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(30));
    if fresh && std::fs::read_to_string(&path).is_ok_and(|prev| prev == payload) {
        return false;
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, payload);
    true
}

/// 엔진이 이번에 넘긴 입력 중 상태줄 mod 가 줄을 다시 지을 때 쓰는 것 — 모델 표시명과 effort 는 mod 가
/// 엔진에 물을 길이 없어(effort 는 첫 턴 전엔 이벤트로도 안 온다) 여기서 마지막 값을 가져간다.
/// `at_ms` 는 엔진이 이 입력을 지은 무렵이라, mod 는 이보다 늦게 안 사실만 덮어쓴다.
fn sl_write_engine_facts(pane: &str, d: &Value, at_ms: u128) {
    let mut facts = serde_json::Map::new();
    for key in ["session_id", "cwd", "model", "effort", "context_window"] {
        if let Some(v) = d.get(key) {
            facts.insert(key.to_string(), v.clone());
        }
    }
    facts.insert("at_ms".to_string(), serde_json::json!(at_ms as u64));
    let dir = std::env::temp_dir().join("kasaterm-statusline");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-engine.json", pane.trim_start_matches('%')));
    let tmp = dir.join(format!("{}-engine.json.{}", pane.trim_start_matches('%'), std::process::id()));
    if std::fs::write(&tmp, Value::Object(facts).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn run_statusline() {
    use std::io::Read;
    let started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let mut buf = String::new();
    let Some(d) = std::io::stdin()
        .read_to_string(&mut buf)
        .ok()
        .and_then(|_| serde_json::from_str::<Value>(&buf).ok())
    else {
        println!("{} err{SL_RESET}", ansi_fg("f7768e"));
        return;
    };

    let cwd = d
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        .unwrap_or_default();
    let session_id = d.get("session_id").and_then(Value::as_str).unwrap_or("");
    let pane = sl_env("KASATERM_PANE_ID");
    // 상태줄 mod 가 바뀐 순간 같은 줄을 지으려 부른 것 — 엔진 입력이 아니라 보고·스냅샷은 엔진이 부른 쪽 몫이다.
    let drawing_only = sl_env("KASATERM_STATUSLINE_DRAW_ONLY").is_some();
    if let (Some(pane), false) = (pane.as_deref(), drawing_only) {
        sl_write_engine_facts(pane, &d, started_ms);
    }

    // claude 내부 cd 와 컨텍스트 창을 GUI 에 보고 — 자기 자신을 report-cwd 로 재실행(비동기).
    // 창을 함께 보내는 이유는 transcript 의 model 에 `[1m]` 이 안 실려 GUI 가 1M 세션을 200k 로
    // 오판하기 때문이다. model 은 `id` **원본** — `[1m]` 을 떼면 복원 때 200k 로 강등된다.
    if let (Some(pane), false, false) = (pane.as_deref(), cwd.is_empty(), drawing_only) {
        let (win, _, tot) = sl_context(&d);
        let report = [
            "report-cwd".to_string(),
            pane.to_string(),
            cwd.clone(),
            session_id.to_string(),
            win.to_string(),
            tot.to_string(),
            d.pointer("/model/id").and_then(Value::as_str).unwrap_or("").to_string(),
            d.pointer("/effort/level").and_then(Value::as_str).unwrap_or("").to_string(),
            sl_model_label(&d, win),
        ];
        if sl_report_due(pane, &report.join("\n")) {
            if let Ok(me) = std::env::current_exe() {
                let _ = std::process::Command::new(me)
                    .args(&report)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
    }

    let mut character = sl_env("KASATERM_CHARACTER");
    // 포크/attach 뷰(세션 id ≠ env anchor)는 env 캐릭터가 출생 pane 의 동결값이라, 그때만
    // 세션→캐릭터 영속 바인딩을 정본으로 읽는다.
    if !session_id.is_empty()
        && std::env::var("KASATERM_SESSION_ID").ok().as_deref() != Some(session_id)
    {
        let config = crate::isolated_collab_root()
            .unwrap_or_else(|| sl_home().join(".config/kasaterm"));
        let path = crate::session_storage::read_path(&config, "session_characters.json");
        if let Some(bound) = sl_read_json(&path)
            .and_then(|map| map.get(session_id).and_then(Value::as_str).map(str::to_string))
            .filter(|s| !s.is_empty())
        {
            character = Some(bound);
        }
    }

    let config = sl_read_json(&sl_home().join(".claude/statusline-config.json")).unwrap_or(Value::Null);
    let settings_path = sl_env("KASATERM_SETTINGS_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| sl_home().join(".config/kasaterm/settings.json"));
    let settings = sl_read_json(&settings_path).unwrap_or(Value::Null);
    let branch = sl_git_branch(&cwd);
    let module = pane.as_deref().and_then(|pane| sl_module_facts(pane, session_id));
    println!(
        "{}",
        sl_line(
            &d,
            &cwd,
            &SlSurroundings {
                in_pane: pane.is_some(),
                character: character.as_deref(),
                config: &config,
                settings: &settings,
                branch: branch.as_deref(),
                module: module.as_ref(),
            },
        )
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn summon_retries_only_while_the_student_is_booting() {
        for booting in [
            "shell or unsupported harness cannot receive tell",
            "bound conversation differs from the live process; refresh before tell",
            "full current Claude session unavailable; tell withheld",
        ] {
            assert!(super::tell_target_booting(booting), "{booting}");
        }
        for final_refusal in ["target pane is closed", "force cannot bypass tell protection", ""] {
            assert!(!super::tell_target_booting(final_refusal), "{final_refusal}");
        }
    }

    #[test]
    fn tell_marks_sender_character_once() {
        assert_eq!(super::mark_tell_sender("본문".into(), Some("아로나")), "⟦아로나⟧ 본문");
        assert_eq!(super::mark_tell_sender("  두 줄\n둘째".into(), Some("아로나")), "⟦아로나⟧ 두 줄\n둘째");
        assert_eq!(super::mark_tell_sender("⟦아로나⟧ 이미".into(), Some("아로나")), "⟦아로나⟧ 이미");
        assert_eq!(super::mark_tell_sender("본문".into(), None), "본문");
        assert_eq!(super::mark_tell_sender("본문".into(), Some(" ")), "본문");
    }

    #[test]
    fn explicit_http_transport_preserves_tell_body_without_a_socket() {
        use super::*;
        use std::io::{Read,Write};
        let mut args = vec!["--api".into(),"http://127.0.0.1:1234".into(),"board".into(),"--all".into()];
        assert!(parse_api_target(&mut args).unwrap().is_some()); assert_eq!(args[0],"board");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = ApiTarget {base:format!("http://{}",listener.local_addr().unwrap()),token_file:None};
        let params = json!({"body":"한글 첫 줄\n\"둘째 줄\"\t끝","message_id":"synthetic","address":{"machine_id":"fake"}});
        let expected = params.clone();
        let server = std::thread::spawn(move || {
            let (mut socket,_) = listener.accept().unwrap();
            socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut bytes = Vec::new(); let mut one = [0u8;1];
            while !bytes.ends_with(b"\r\n\r\n") { socket.read_exact(&mut one).unwrap(); bytes.push(one[0]); }
            let headers = String::from_utf8(bytes).unwrap();
            assert!(headers.starts_with("POST /collab/tell "));
            let length = headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n|n.trim().parse::<usize>().ok())).unwrap();
            let mut body = vec![0;length]; socket.read_exact(&mut body).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),expected);
            let body = json!({"accepted":true}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let response = api_roundtrip(&target,&Request{id:json!("test"),method:"collab.tell".into(),params}).unwrap();
        assert!(response.ok); assert_eq!(response.result.unwrap()["accepted"],true);
        server.join().unwrap();
        assert!(api_roundtrip(&target,&Request{id:json!("test"),method:"surface.send".into(),params:json!({})}).is_err());
    }

    /// 오케스트레이터 보고는 origin env 가 없으면 파라미터도 못 만든다 — 사람이 손수 띄운
    /// 학생이 쳐도 보고함 근처에 못 간다. 있으면 env 의 conv/task/machine 이 실린다.
    #[test]
    fn orchestrator_report_requires_origin_env_and_carries_it() {
        let none = |_: &str| None::<String>;
        let err = super::orchestrator_report_params(&["--status".into(),"done".into(),"--summary".into(),"x".into()], &none).unwrap_err().to_string();
        assert!(err.contains("KASATERM_ORIGIN"), "{err}");
        let env = |k: &str| match k {
            "KASATERM_ORIGIN" => Some("nacho".to_string()),
            "KASATERM_ORIGIN_CONV" => Some("discord:42".to_string()),
            "KASATERM_ORIGIN_TASK" => Some("t-7".to_string()),
            "KASATERM_ORIGIN_MACHINE" => Some("mini-id".to_string()),
            "KASATERM_PANE_ID" => Some("%9".to_string()),
            "KASATERM_CHARACTER" => Some("와카모".to_string()),
            "KASATERM_MACHINE_ID" => Some("student-id".to_string()),
            "KASATERM_SELF_LABEL" => Some("맥북".to_string()),
            "CLAUDECODE" => Some("1".to_string()),
            _ => None,
        };
        let args: Vec<String> = ["--status","needs_restart","--summary","셀프케어 고침","--changed","selfcare.sh, main.py","--changed","worklog.py","--tests","pytest 3 ok","--next","재시작 뒤 E2E"].iter().map(|s|s.to_string()).collect();
        let p = super::orchestrator_report_params(&args, &env).unwrap();
        assert_eq!(p["origin"],"nacho"); assert_eq!(p["conv"],"discord:42"); assert_eq!(p["task_id"],"t-7");
        assert_eq!(p["machine_id"],"mini-id"); assert_eq!(p["surface"],"%9"); assert_eq!(p["character"],"와카모");
        assert_eq!(p["harness"],"claude"); assert_eq!(p["host"]["machine_id"],"student-id"); assert_eq!(p["host"]["label"],"맥북");
        assert_eq!(p["changed"],json!(["selfcare.sh","main.py","worklog.py"]));
        let envelope = crate::nacho_inbox::build(&p).unwrap();
        assert_eq!(envelope["status"],"needs_restart");
        assert!(super::orchestrator_report_params(&["--bogus".into()], &env).is_err());
        assert_eq!(p["run_id"], "", "세대 env 가 없으면 빈 칸");
        let with_run = |k: &str| if k == "KASATERM_ORIGIN_RUN" { Some("t-7.r3".to_string()) } else { env(k) };
        assert_eq!(super::orchestrator_report_params(&args, &with_run).unwrap()["run_id"], "t-7.r3");
        let mut flagged = args.clone();
        flagged.extend(["--run".to_string(), "t-8.r1".to_string()]);
        assert_eq!(super::orchestrator_report_params(&flagged, &with_run).unwrap()["run_id"], "t-8.r1", "브리프가 준 --run 이 env 를 이긴다");
        // --api 매핑: 원격 기계로 갈 때 HTTP 로도 같은 메서드가 간다.
        let mut args = vec!["--api".into(),"http://127.0.0.1:1".into(),"board".into()];
        assert!(super::parse_api_target(&mut args).unwrap().is_some());
    }

    #[test]
    fn surface_key_comes_from_the_local_board_only_when_the_number_is_unique() {
        let snap = json!({"panes":[
            {"address":{"surface_id":"%1","surface_key":"k-1"}},
            {"address":{"surface_id":"%2","surface_key":"k-2a"}},
            {"address":{"surface_id":"%2","surface_key":"k-2b"}},
        ]});
        assert_eq!(super::surface_key_in(Some(&snap), "%1"), "k-1");
        assert_eq!(super::surface_key_in(Some(&snap), "%2"), "", "같은 번호가 둘이면 고르지 않는다");
        assert_eq!(super::surface_key_in(Some(&snap), "%9"), "");
        assert_eq!(super::surface_key_in(None, "%1"), "", "판을 못 읽어도 보고는 간다");
    }

    #[test]
    fn board_scope_uses_shared_collector_and_legacy_stays_compatible() {
        let all = super::build_request("board", &["--all".into()]).unwrap();
        assert_eq!(all.method,"collab.snapshot"); assert_eq!(all.params["scope"],"all");
        let local = super::build_request("board", &["--local".into()]).unwrap();
        assert_eq!(local.params["scope"],"local");
        assert_eq!(super::build_request("board", &[]).unwrap().method,"collab.board");
        assert_eq!(super::build_request("board", &["12".into()]).unwrap().params["screen_lines"],12);
        assert!(super::build_request("board", &["--all".into(),"12".into()]).is_err());
        let address = r#"{"machine_id":"machine","surface_key":"key","surface_id":"%1"}"#;
        let inspect = super::build_request("activity", &["--address".into(),address.into(),"999".into()]).unwrap();
        assert_eq!(inspect.method,"collab.inspect"); assert_eq!(inspect.params["limit"],50);
        assert_eq!(super::build_request("activity", &["%1".into()]).unwrap().method,"collab.activity");
    }
    use super::*;

    #[test]
    fn server_preserves_shell_command_and_registration_mode() {
        let command = "printf '%s\\n' \"$VALUE\" 'space in argument'";
        let args = [
            "--surface",
            "%1",
            "--cwd",
            "/tmp/a b",
            "--name",
            "local",
            "--register-only",
            command,
        ]
        .map(String::from);
        let params = server_params(&args).unwrap();
        assert_eq!(params["command"], command);
        assert_eq!(params["cwd"], "/tmp/a b");
        assert_eq!(params["start"], false);
        let args = ["--surface", "%1", "npm run dev"].map(String::from);
        assert_eq!(server_params(&args).unwrap()["start"], true);
    }

    #[test]
    fn server_clear_cannot_accidentally_execute_and_requires_explicit_target() {
        let args = ["--surface", "%1", "--clear"].map(String::from);
        assert_eq!(
            server_params(&args).unwrap(),
            json!({"surface":"%1","clear":true})
        );
        for args in [
            vec!["--clear"],
            vec!["--surface", "%1", "--clear", "npm run dev"],
            vec!["--surface", "%1", "npm", "run", "dev"],
        ] {
            assert!(
                server_params(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
    }

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

    #[test]
    fn wait_since_reads_epoch_ms_or_tell_receipt() {
        assert_eq!(super::since_ms("1790589573069"), Some(1_790_589_573_069));
        assert_eq!(super::since_ms("kt1.1790589573069.10336-18d973bad15450f0-0"), Some(1_790_589_573_069));
        assert_eq!(super::since_ms("미도리"), None);
    }

    #[test]
    fn summon_quotes_the_folder_for_the_shell() {
        assert_eq!(super::shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(super::shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn summon_hands_codex_the_brief_as_its_first_input_only_for_a_fresh_conversation() {
        for boot in ["codex", "시로코 codex", "kimi codex", "codex --model gpt-5"] {
            assert!(super::boots_fresh_codex(boot), "{boot}");
        }
        for boot in ["claude", "시로코", "codex resume abc", "codex exec 'x'", "codex '이미 준 지시'", "codexx"] {
            assert!(!super::boots_fresh_codex(boot), "{boot}");
        }
    }

    #[test]
    fn where_lists_cells_and_tabs_and_finds_by_any_name() {
        let resp: super::Response = serde_json::from_value(serde_json::json!({"id":"t","ok":true,"result":{"rooms":[
            {"window":0,"label":"kasaterm","active":true,"cells":[
                {"cell":"%1","row":1,"col":1,"x":0,"y":0,"w":50,"h":100,"tabs":[
                    {"n":1,"active":false,"kind":"terminal","surface":"%1","character":"유우카","title":"미러링"},
                    {"n":2,"active":true,"kind":"web","title":"Example","url":"https://example.com"}]},
                {"cell":"%4","row":1,"col":2,"x":50,"y":0,"w":50,"h":100,"tabs":[{"n":1,"active":true,"kind":"terminal","surface":"%4","character":"시로코"}]}]}]}}))
            .unwrap();
        let all = super::render_where(&resp, "", Some("%4"));
        assert!(all.starts_with("방 1 「kasaterm」 (보는 중)"), "{all}");
        assert!(all.contains("┌") && all.contains("%4"), "방마다 배치도를 먼저 그린다: {all}");
        assert!(all.contains("1행 1열(%1) · 탭 1/2(뒤) · 터미널 %1 · 유우카 · 미러링"), "{all}");
        assert!(all.contains("1행 2열(%4) · 터미널 %4 · 시로코  ← 나"), "{all}");
        let web = super::render_where(&resp, "example", None);
        assert_eq!(web, "방 1 「kasaterm」 (보는 중) · 1행 1열(%1) · 탭 2/2 · 웹 · Example · https://example.com");
        assert!(super::render_where(&resp, "없는학생", None).contains("맞는 칸·탭이 없어요"));
    }

    #[test]
    fn merged_commands_map_to_the_same_socket_calls() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let raw = super::build_request("tell:raw", &v(&["%3", "cd", "/x"])).unwrap();
        assert_eq!((raw.method.as_str(), raw.params["surface_id"].as_str(), raw.params["text"].as_str()),
            ("surface.send_text", Some("%3"), Some("cd /x")));
        let key = super::build_request("tell:key", &v(&["--surface", "%4", "enter"])).unwrap();
        assert_eq!((key.method.as_str(), key.params["key"].as_str()), ("surface.send_key", Some("enter")));
        assert!(super::build_request("tell:raw", &v(&["%3"])).is_err(), "넣을 글이 없으면 거부");
        let title = super::build_request("rename-window", &v(&["%5", "지금 일"])).unwrap();
        assert_eq!((title.method.as_str(), title.params["title"].as_str()), ("surface.rename", Some("지금 일")));
        let color = super::build_request("rename-window", &v(&["%5", "--color", "#ff0000"])).unwrap();
        assert_eq!((color.method.as_str(), color.params["color"].as_str()), ("surface.set_color", Some("#ff0000")));
        assert!(super::build_request("rename-window", &v(&["%5", "두", "토막"])).is_err(), "따옴표 빠진 제목은 거부");
        let server = super::build_request("tab:server", &v(&["--surface", "%2", "--clear"])).unwrap();
        assert_eq!(server.method, "surface.server");
    }

    #[test]
    fn done_splits_board_outcome_and_orchestrator_report() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (board, report) = super::split_done_args(&v(&["succeeded", "다", "했다", "--tests", "12 ok"])).unwrap();
        assert_eq!(board, v(&["succeeded", "다 했다"]));
        assert_eq!(report, v(&["--tests", "12 ok", "--status", "done", "--summary", "다 했다"]));
        let (board, report) = super::split_done_args(&v(&["--surface", "%7", "needs_restart", "훅 고침", "--next", "재시작", "--task", "t-1"])).unwrap();
        assert!(report.windows(2).any(|w| w == ["--task", "t-1"]), "브리프가 준 일 번호는 보고로");
        assert_eq!(board, v(&["--surface", "%7", "failed", "재시작 필요: 훅 고침"]));
        assert!(report.windows(2).any(|w| w == ["--status", "needs_restart"]));
        let (_, report) = super::split_done_args(&v(&["failed"])).unwrap();
        assert_eq!(&report[report.len() - 4..], &v(&["--status", "blocked", "--summary", "blocked"])[..]);
        assert!(super::split_done_args(&v(&["maybe"])).is_err());
        assert!(super::split_done_args(&v(&["--next"])).is_err());
    }

    #[test]
    fn summon_takes_the_purpose_line_as_current_work() {
        let brief = "작업현황 구현을 맡아 줘.\n\n목적: 사이드바 현황 목록\n파일: render.rs";
        assert_eq!(super::brief_title(brief).as_deref(), Some("사이드바 현황 목록"));
        assert_eq!(super::brief_title("  목적 :  세션 이름\n").as_deref(), Some("세션 이름"));
        assert_eq!(super::brief_title("목적이 뭔지 모르겠다"), None, "「목적:」 줄이 아니면 제목을 안 바꾼다");
        assert_eq!(super::brief_title("목적:   \n"), None);
    }

    #[test]
    fn tell_refuses_unknown_options_instead_of_sending_them_as_the_body() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let address = r#"{"machine_id":"m","surface_key":"k","surface_id":"%1","session_id":"s","instance_id":"i"}"#;
        // 2026-10-01: `--title` 을 모르던 판이 `--title 제목 --stdin` 을 본문으로 보냈다 — 지금은 모르는 옵션이면 멈춘다.
        let err = super::build_request("tell", &v(&["--address", address, "--titel", "보드 걷기", "--stdin"])).unwrap_err();
        assert!(err.to_string().contains("모르는 옵션 --titel"), "{err}");
        let err = super::build_request("tell", &v(&["%1", "본문", "--title", "늦은 제목"])).unwrap_err();
        assert!(err.to_string().contains("본문 뒤"), "{err}");
        let ok = super::build_request("tell", &v(&["%1", "--title", "보드 걷기", "본문", "한 줄"])).unwrap();
        assert_eq!(ok.params["title"], "보드 걷기");
        assert!(ok.params["body"].as_str().unwrap().ends_with("본문 한 줄"));
        let dashed = super::build_request("tell", &v(&["%1", "--", "--로 시작하는 본문"])).unwrap();
        assert!(dashed.params["body"].as_str().unwrap().ends_with("--로 시작하는 본문"));
        assert!(super::build_request("tell", &v(&["%1", "옵션 --title 이야기"])).is_ok(), "한 덩어리 본문 속 낱말은 옵션이 아니다");
    }

    #[test]
    fn a_held_tell_says_why_and_until_when() {
        let receipt = serde_json::json!({"state":"accepted","reason":crate::tell::Hold::Draft.reason(),"expires_at_ms":0});
        let line = super::tell_state_line(&receipt);
        assert!(line.contains("쓰던 글") && line.contains("비우면") && line.contains("버려진다"), "{line}");
        let old = serde_json::json!({"state":"accepted","reason":"no bytes written; waiting"});
        assert!(super::tell_state_line(&old).starts_with("아직 큐"));
    }

    #[test]
    fn board_matches_by_name_machine_or_surface_with_a_session() {
        let rows = vec![
            serde_json::json!({"character":"미도리","machine_label":"맥미니","address":{"surface_id":"%3","session_id":"s1"}}),
            serde_json::json!({"character":"미도리","machine_label":"맥북","address":{"surface_id":"%5","session_id":"s2"}}),
            serde_json::json!({"character":"유즈","machine_label":"맥북","address":{"surface_id":"%6"}}),
        ];
        assert_eq!(super::board_matches(&rows, "미도리").len(), 2);
        assert_eq!(super::board_matches(&rows, "미도리@맥미니").len(), 1);
        let nbsp = vec![serde_json::json!({"character":"유우카","machine_label":"건호의 MacBook\u{a0}Pro","address":{"surface_id":"%3","session_id":"s"}})];
        assert_eq!(super::board_matches(&nbsp, "유우카@건호의 MacBook Pro").len(), 1, "줄바꿈 없는 공백도 띄어쓰기로 본다");
        assert_eq!(super::board_matches(&rows, "%5").len(), 1);
        assert!(super::board_matches(&rows, "유즈").is_empty());
    }

    fn statusline(d: &serde_json::Value, in_pane: bool, character: Option<&str>, settings: &serde_json::Value) -> String {
        super::sl_line(
            d,
            "/tmp/hidden-directory",
            &super::SlSurroundings {
                in_pane,
                character,
                config: &serde_json::Value::Null,
                settings,
                branch: None,
                module: None,
            },
        )
    }

    #[test]
    fn module_parts_show_only_cost() {
        let m = serde_json::json!({"limits": [{"kind": "five_hour", "percent": 93.0}, {"kind": "seven_day", "percent": 12.0}], "cost_usd": 1.234, "background": 2});
        let plain: Vec<String> = super::sl_module_parts(&m).iter().map(|p| super::strip_ansi_for_test(p)).collect();
        assert_eq!(plain, ["$1.23"]);
        assert!(super::sl_module_parts(&serde_json::json!({"limits": [], "cost_usd": 0.0, "background": 0})).is_empty());
    }

    #[test]
        fn hidden_statusline_fields_keep_the_pane_marker() {
        let d = serde_json::json!({"model": {"id": "claude-fixture", "display_name": "FixtureModel"}, "context_window": {"used_percentage": 23}});
        let off = serde_json::json!({"agent_statusline_model": false, "agent_statusline_usage": false, "agent_statusline_cwd": false});
        assert_eq!(statusline(&d, true, Some("프라나"), &off), super::SL_SPRITE);
    }

    #[test]
    fn missing_or_non_boolean_statusline_preferences_show_the_field() {
        let d = serde_json::json!({"model": {"id": "claude-fixture", "display_name": "FixtureModel"}, "context_window": {"used_percentage": 23}});
        for settings in [serde_json::Value::Null, serde_json::json!("invalid"), serde_json::json!({"agent_statusline_usage": "false"})] {
            let line = statusline(&d, false, None, &settings);
            assert!(line.contains("FixtureModel") && line.contains("hidden-directory") && line.contains("23%"), "{line}");
        }
    }

    #[test]
    fn statusline_model_marker_only_inside_a_pane() {
        for (id, marker) in [("claude-opus-5-5[1m]", super::SL_MODEL_MARKER_CLAUDE), ("gpt-5.5", super::SL_MODEL_MARKER_GPT), ("o3", super::SL_MODEL_MARKER_GPT)] {
            let d = serde_json::json!({"model": {"id": id, "display_name": "M"}});
            assert!(statusline(&d, true, None, &serde_json::Value::Null).contains(marker), "{id}");
            assert!(!statusline(&d, false, None, &serde_json::Value::Null).contains(marker), "{id}");
        }
    }

    #[test]
    fn engine_facts_keep_only_what_the_mod_redraws_with() {
        let pane = format!("%engine-test-{}", std::process::id());
        let d = serde_json::json!({
            "session_id": "s1", "cwd": "/w", "transcript_path": "/secret.jsonl",
            "model": {"id": "claude-opus-5-5[1m]", "display_name": "Opus 5.5 (1M context)"},
            "effort": {"level": "xhigh"}, "context_window": {"used_percentage": 12},
        });
        super::sl_write_engine_facts(&pane, &d, 1234);
        let path = std::env::temp_dir().join("kasaterm-statusline").join(format!("{}-engine.json", pane.trim_start_matches('%')));
        let facts = super::sl_read_json(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(facts["at_ms"], 1234);
        assert_eq!(facts["model"]["display_name"], "Opus 5.5 (1M context)");
        assert_eq!(facts["effort"]["level"], "xhigh");
        assert!(facts.get("transcript_path").is_none(), "줄 짓기에 안 쓰는 값은 안 남긴다");
    }

    #[test]
    fn statusline_branch_reads_head_without_git() {
        let root = std::env::temp_dir().join(format!("kt-sl-branch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("deep/er")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/rain\n").unwrap();
        assert_eq!(super::sl_git_branch(repo.join("deep/er").to_str().unwrap()).as_deref(), Some("feat/rain"));

        let admin = repo.join(".git/worktrees/side");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(admin.join("HEAD"), "3f2a9c0000000000000000000000000000000000\n").unwrap();
        let side = root.join("side");
        std::fs::create_dir_all(&side).unwrap();
        std::fs::write(side.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert_eq!(super::sl_git_branch(side.to_str().unwrap()).as_deref(), Some("HEAD"));

        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let above_is_repo = plain.ancestors().skip(1).any(|dir| dir.join(".git").exists());
        if !above_is_repo {
            assert_eq!(super::sl_git_branch(plain.to_str().unwrap()), None);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
