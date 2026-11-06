//! 실제 어댑터로 한 턴을 돌려 이벤트를 한 줄씩 찍는다.
//!
//!   cargo run -p kasa-acp --example acp_turn -- codex /abs/dir "할 일"
//!   CLAUDE_BIN=/path/to/claude cargo run -p kasa-acp --example acp_turn -- claude /abs/dir "할 일" --allow
//!
//! 권한 요청은 `--allow` 면 한 번 허락하고, 아니면 거절한다.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use kasa_acp::{Connection, Event, Harness, LaunchSpec, PermissionKind, ToolOutput};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: acp_turn claude|codex <absolute dir> <prompt> [--allow]";
    let harness = match args.first().map(String::as_str) {
        Some("claude") => Harness::Claude,
        Some("codex") => Harness::Codex,
        _ => return Err(usage.into()),
    };
    let cwd = PathBuf::from(args.get(1).ok_or(usage)?);
    let prompt = args.get(2).ok_or(usage)?;
    let allow = args.iter().any(|a| a == "--allow");

    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut spec = LaunchSpec::resolve(harness, &path).ok_or("neither the adapter nor npx is on PATH")?;
    if let (Harness::Claude, Some(claude)) = (harness, std::env::var_os("CLAUDE_BIN")) {
        spec = spec.claude_executable(Path::new(&claude));
    }
    eprintln!("launch: {} {:?}", spec.program.display(), spec.args);

    let (conn, events) = Connection::spawn(&spec, &cwd)?;
    let info = conn.initialize()?;
    eprintln!("agent: {} {}", info["agentInfo"]["name"], info["agentInfo"]["version"]);
    let session = conn.new_session(&cwd)?;
    eprintln!("session: {} mode={:?}", session.session_id, session.current_mode);

    let pending = conn.prompt(&session.session_id, prompt);
    let mut in_message = false;
    let result = loop {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => show(&conn, ev, allow, &mut in_message)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Err("event stream ended".into()),
        }
        if let Some(r) = pending.try_take() {
            while let Ok(ev) = events.try_recv() {
                show(&conn, ev, allow, &mut in_message)?;
            }
            break r.map_err(Box::<dyn std::error::Error>::from);
        }
    };
    if in_message {
        println!();
    }
    match result {
        Ok(v) => println!("outcome: {:?}", Connection::prompt_outcome(&v)?),
        Err(e) => println!("failed: {e}\nstderr: {}", conn.stderr_tail()),
    }
    conn.shutdown(Duration::from_secs(5));
    Ok(())
}

fn show(conn: &Connection, ev: Event, allow: bool, in_message: &mut bool) -> Result<(), kasa_acp::Error> {
    if let Event::Message { text, .. } = &ev {
        print!("{text}");
        std::io::stdout().flush().ok();
        *in_message = true;
        return Ok(());
    }
    if std::mem::take(in_message) {
        println!();
    }
    match ev {
        Event::ToolCall(c) => println!("[tool] {} {:?} {}", c.id, c.status, c.command.unwrap_or(c.title)),
        Event::ToolCallUpdate(u) => {
            let out = match u.output {
                Some(ToolOutput::Delta(s) | ToolOutput::Full(s)) => s.lines().next().unwrap_or_default().to_owned(),
                None => String::new(),
            };
            println!("[tool] {} {:?} exit={:?} {out}", u.id, u.status, u.exit_code);
        }
        Event::Usage { used, size, cost_usd, .. } => {
            println!("[context] {used}/{size} ({:.1}%) cost={cost_usd:?}", used as f64 * 100.0 / size as f64);
        }
        Event::Permission(req) => {
            let kind = if allow { PermissionKind::AllowOnce } else { PermissionKind::RejectOnce };
            let choice = req.option(kind).map(|o| o.id.clone());
            println!("[permission] {:?} -> {choice:?}", req.title);
            conn.answer_permission(&req.id, choice.as_deref())?;
        }
        Event::Other { kind, .. } => println!("[{kind}]"),
        other => println!("{other:?}"),
    }
    Ok(())
}
