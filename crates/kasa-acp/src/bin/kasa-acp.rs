//! `kasa-acp run` — 칸 안에서 ACP 학생 하나를 돌린다.
//!
//!   kasa-acp run --harness claude|codex [--cwd DIR] [--mode ID] [--set ID=VALUE]...
//!                [--status FILE] [--inbox FILE] [--resume SESSION]
//!                [--permission ask|allow|reject] [--once] [-- 첫 지시]
//!
//! 턴이 시작하고 끝날 때마다 `--status` 파일을 덮어쓴다. 디스패처는 그 파일로 수확한다 — 화면의
//! idle 을 짐작하지 않는다. `--once` 가 아니면 턴 뒤에 다음 지시를 칸 입력이나 `--inbox` 파일로 받는다.
//! Ctrl-C 는 도는 턴을 멈추고, 멈추는 중에 한 번 더 누르면 끝낸다.

use std::error::Error;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kasa_acp::runner::{self, Line, SessionView};
use kasa_acp::{Connection, Event, Harness, LaunchSpec, PermissionKind, PromptOutcome, find_program};

const TICK: Duration = Duration::from_millis(100);
const USAGE: &str = "usage: kasa-acp run --harness claude|codex [--cwd DIR] [--mode ID] [--set ID=VALUE]... \
[--status FILE] [--inbox FILE] [--resume SESSION] [--permission ask|allow|reject] [--once] [-- PROMPT]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    Ask,
    Allow,
    Reject,
}

struct Args {
    harness: Harness,
    cwd: PathBuf,
    mode: Option<String>,
    set: Vec<(String, String)>,
    status: Option<PathBuf>,
    inbox: Option<PathBuf>,
    resume: Option<String>,
    permission: Policy,
    once: bool,
    prompt: Option<String>,
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    if it.next().as_deref() != Some("run") {
        return Err(USAGE.into());
    }
    let mut harness = None;
    let mut cwd = None;
    let mut mode = None;
    let mut set = Vec::new();
    let mut status = None;
    let mut inbox = None;
    let mut resume = None;
    let mut permission = Policy::Ask;
    let mut once = false;
    let mut words: Vec<String> = Vec::new();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--harness" => {
                harness = Some(match value()?.as_str() {
                    "claude" => Harness::Claude,
                    "codex" => Harness::Codex,
                    other => return Err(format!("unknown harness {other}")),
                });
            }
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--mode" => mode = Some(value()?),
            "--set" => {
                let kv = value()?;
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("--set wants ID=VALUE, got {kv}"))?;
                set.push((k.to_owned(), v.to_owned()));
            }
            "--status" => status = Some(PathBuf::from(value()?)),
            "--inbox" => inbox = Some(PathBuf::from(value()?)),
            "--resume" => resume = Some(value()?),
            "--permission" => {
                permission = match value()?.as_str() {
                    "ask" => Policy::Ask,
                    "allow" => Policy::Allow,
                    "reject" => Policy::Reject,
                    other => return Err(format!("unknown permission policy {other}")),
                };
            }
            "--once" => once = true,
            "--" => {
                words.extend(it.by_ref());
                break;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}\n{USAGE}")),
            _ => words.push(arg),
        }
    }
    let cwd = match cwd {
        Some(dir) => std::path::absolute(dir).map_err(|e| e.to_string())?,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    // 사람이 없으면 묻는 정책은 영원히 기다린다.
    if permission == Policy::Ask && !std::io::stdin().is_terminal() {
        permission = Policy::Reject;
    }
    Ok(Args {
        harness: harness.ok_or(USAGE)?,
        cwd,
        mode,
        set,
        status,
        inbox,
        resume,
        permission,
        once,
        prompt: (!words.is_empty()).then(|| words.join(" ")),
    })
}

static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTS.fetch_add(1, Ordering::Relaxed);
}

fn install_sigint() {
    #[cfg(unix)]
    // SAFETY: 처리기는 원자 카운터 하나만 올린다(async-signal-safe).
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
}

fn take_interrupt() -> bool {
    INTERRUPTS.swap(0, Ordering::Relaxed) > 0
}

fn stdin_lines() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { return };
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    rx
}

struct Screen {
    styled: bool,
    at_line_start: bool,
}

impl Screen {
    fn put(&mut self, line: &Line) {
        match line {
            Line::Text(t) => {
                print!("{t}");
                self.at_line_start = t.ends_with('\n');
            }
            Line::Block(b) => {
                self.break_line();
                println!("{b}");
            }
        }
        std::io::stdout().flush().ok();
    }

    fn note(&mut self, text: &str) {
        self.break_line();
        if self.styled {
            println!("\x1b[2m{text}\x1b[0m");
        } else {
            println!("{text}");
        }
    }

    fn break_line(&mut self) {
        if !self.at_line_start {
            println!();
            self.at_line_start = true;
        }
    }
}

enum TurnEnd {
    Done(PromptOutcome),
    /// 연결이 죽었거나 사람이 두 번 멈췄다. 결과 파일에 까닭을 남기고 끝낸다.
    Broken(String),
}

struct Session<'a> {
    conn: &'a Connection,
    events: &'a Receiver<Event>,
    lines: &'a Receiver<String>,
    inbox: Option<&'a std::path::Path>,
    id: &'a str,
    policy: Policy,
}

impl Session<'_> {
    fn turn(&self, prompt: &str, view: &mut SessionView, screen: &mut Screen) -> TurnEnd {
        let pending = self.conn.prompt(self.id, prompt);
        let mut cancelling = false;
        loop {
            if take_interrupt() {
                if cancelling {
                    return TurnEnd::Broken("interrupted".into());
                }
                cancelling = true;
                screen.note("(cancelling — Ctrl-C again to quit)");
                if self.conn.cancel(self.id).is_err() {
                    return TurnEnd::Broken("agent disconnected".into());
                }
            }
            match self.events.recv_timeout(TICK) {
                Ok(ev) => {
                    if let Some(end) = self.show(ev, view, screen) {
                        return end;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return TurnEnd::Broken("agent disconnected".into()),
            }
            if let Some(result) = pending.try_take() {
                while let Ok(ev) = self.events.try_recv() {
                    self.show(ev, view, screen);
                }
                return match result.and_then(|v| Connection::prompt_outcome(&v)) {
                    Ok(outcome) => TurnEnd::Done(outcome),
                    Err(e) => TurnEnd::Broken(format!("{e}\n{}", self.conn.stderr_tail().trim())),
                };
            }
        }
    }

    fn show(&self, ev: Event, view: &mut SessionView, screen: &mut Screen) -> Option<TurnEnd> {
        match ev {
            Event::Permission(req) => {
                let choice = match self.policy {
                    Policy::Allow => req.option(PermissionKind::AllowOnce).map(|o| o.id.clone()),
                    Policy::Reject => req.option(PermissionKind::RejectOnce).map(|o| o.id.clone()),
                    Policy::Ask => self.ask(&req, screen),
                };
                if self.conn.answer_permission(&req.id, choice.as_deref()).is_err() {
                    return Some(TurnEnd::Broken("agent disconnected".into()));
                }
            }
            Event::Closed => {
                return Some(TurnEnd::Broken(format!("agent exited\n{}", self.conn.stderr_tail().trim())));
            }
            ev => {
                for line in view.observe(&ev) {
                    screen.put(&line);
                }
            }
        }
        None
    }

    fn ask(&self, req: &kasa_acp::PermissionRequest, screen: &mut Screen) -> Option<String> {
        let (text, ids) = runner::permission_prompt(req);
        screen.break_line();
        println!("{text}");
        loop {
            print!("> ");
            std::io::stdout().flush().ok();
            let answer = loop {
                if take_interrupt() {
                    return None;
                }
                match self.lines.recv_timeout(TICK) {
                    Ok(line) => break line,
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return None,
                }
            };
            if let Some(id) = answer.trim().parse::<usize>().ok().and_then(|n| ids.get(n.wrapping_sub(1))) {
                return Some(id.clone());
            }
        }
    }

    /// 다음 지시 — 칸 입력이나 inbox 파일 중 먼저 온 것. 입력이 끝났거나 Ctrl-C 면 `None`.
    fn next_prompt(&self, screen: &mut Screen) -> Option<String> {
        screen.break_line();
        print!("\n> ");
        std::io::stdout().flush().ok();
        let mut typed_closed = false;
        let mut warned = false;
        loop {
            if take_interrupt() {
                return None;
            }
            match self.inbox.map(runner::take_inbox) {
                Some(Ok(Some(text))) => {
                    let first = text.lines().next().unwrap_or_default();
                    let more = if text.lines().nth(1).is_some() { " …" } else { "" };
                    println!("{first}{more}");
                    return Some(text);
                }
                Some(Err(e)) if !warned => {
                    warned = true;
                    screen.note(&format!("(inbox unreadable: {e})"));
                }
                Some(Err(_)) => {}
                Some(Ok(None)) | None => {}
            }
            if typed_closed {
                // 칸 입력이 닫혀도 inbox 로 오는 지시는 계속 받는다. inbox 도 없으면 끝낼 길뿐이다.
                if self.inbox.is_none() {
                    return None;
                }
                std::thread::sleep(TICK);
                continue;
            }
            match self.lines.recv_timeout(TICK) {
                Ok(line) if line.trim().is_empty() => {
                    print!("> ");
                    std::io::stdout().flush().ok();
                }
                Ok(line) => return Some(line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => typed_closed = true,
            }
        }
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or_default()
}

fn run(args: &Args) -> Result<bool, Box<dyn Error>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut spec = LaunchSpec::resolve(args.harness, &path).ok_or("neither the adapter nor npx is on PATH")?;
    if args.harness == Harness::Claude && std::env::var_os("CLAUDE_CODE_EXECUTABLE").is_none() {
        let claude = find_program("claude", &path).ok_or("claude is not on PATH")?;
        spec = spec.claude_executable(&claude);
    }
    let mut screen = Screen { styled: std::io::stdout().is_terminal(), at_line_start: true };
    screen.note(&format!("starting {} adapter…", args.harness.name()));

    let (conn, events) = Connection::spawn(&spec, &args.cwd)?;
    let info = conn
        .initialize()
        .map_err(|e| format!("{e}\n{}", conn.stderr_tail().trim()))?;
    let session_id = match &args.resume {
        Some(id) => {
            conn.resume_session(id, &args.cwd)?;
            id.clone()
        }
        None => conn.new_session(&args.cwd)?.session_id,
    };
    if let Some(mode) = &args.mode {
        conn.set_mode(&session_id, mode)?;
    }
    for (id, value) in &args.set {
        conn.set_config_option(&session_id, id, value)?;
    }
    let agent = &info["agentInfo"];
    screen.note(&format!(
        "● {} {} · session {session_id} · {}",
        agent["title"].as_str().or(agent["name"].as_str()).unwrap_or("agent"),
        agent["version"].as_str().unwrap_or_default(),
        args.cwd.display()
    ));

    install_sigint();
    let lines = stdin_lines();
    let session = Session {
        conn: &conn,
        events: &events,
        lines: &lines,
        inbox: args.inbox.as_deref(),
        id: &session_id,
        policy: args.permission,
    };
    let mut view = SessionView::default();
    let report = |view: &SessionView, state: &str, stop: Option<&str>, error: Option<&str>| -> Result<(), Box<dyn Error>> {
        let Some(path) = &args.status else { return Ok(()) };
        let mut status = view.status_json(args.harness.name(), &session_id, state, stop, now());
        if let Some(why) = error {
            status["error"] = why.into();
        }
        runner::write_status(path, &status)?;
        Ok(())
    };
    report(&view, "idle", None, None)?;
    let mut next = args.prompt.clone();
    let mut last_stop: Option<String> = None;
    let mut ok = true;
    loop {
        let prompt = match next.take() {
            Some(p) => p,
            None if args.once => break,
            None => match session.next_prompt(&mut screen) {
                Some(p) => p,
                None => break,
            },
        };
        view.begin_turn();
        report(&view, "working", last_stop.as_deref(), None)?;
        let (stop, broken) = match session.turn(&prompt, &mut view, &mut screen) {
            TurnEnd::Done(outcome) => {
                screen.note(&view.footer(&outcome));
                (outcome.stop.as_str().to_owned(), None)
            }
            TurnEnd::Broken(why) => {
                screen.note(&format!("── stopped: {why}"));
                ("error".to_owned(), Some(why))
            }
        };
        if let Some(why) = &broken {
            report(&view, "exited", Some(&stop), Some(why))?;
            ok = false;
            break;
        }
        report(&view, "idle", Some(&stop), None)?;
        last_stop = Some(stop);
        if args.once {
            break;
        }
    }
    if ok {
        report(&view, "exited", last_stop.as_deref(), None)?;
    }
    conn.shutdown(Duration::from_secs(5));
    Ok(ok)
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("kasa-acp: {e}");
            if let Some(path) = &args.status {
                let failed = serde_json::json!({ "harness": args.harness.name(), "state": "exited",
                                                 "stop": "error", "error": e.to_string(), "at": now() });
                let _ = runner::write_status(path, &failed);
            }
            ExitCode::from(1)
        }
    }
}
