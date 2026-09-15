//! Compile only into an isolated test directory, with executable name `claude` or `codex`.
//! This is a byte-recording fixture, never a real model process.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, PartialEq)]
enum Harness {
    Claude,
    Codex,
}

impl Harness {
    fn name(self) -> &'static str {
        if self == Self::Codex {
            "codex"
        } else {
            "claude"
        }
    }
    fn prompt(self) -> &'static str {
        if self == Self::Codex {
            "›"
        } else {
            "❯"
        }
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn write_json(root: &Path, name: &str, value: &str) {
    let temporary = root.join(format!(".{name}.tmp"));
    fs::write(&temporary, value).unwrap();
    fs::rename(temporary, root.join(name)).unwrap();
}

fn escaped(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn input_box(out: &mut impl Write, harness: Harness, content: &str) {
    let columns = std::process::Command::new("/bin/stty")
        .arg("size")
        .stdin(std::process::Stdio::inherit())
        .output()
        .ok()
        .and_then(|value| String::from_utf8(value.stdout).ok())
        .and_then(|value| {
            value
                .split_whitespace()
                .nth(1)
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap_or(90)
        .clamp(20, 500);
    let footer = if harness == Harness::Claude {
        "  bypass permissions on (shift+tab to cycle)"
    } else {
        "  gpt-5.5 medium · fixture · main · Ask for approval · Context 3% used"
    };
    let _ = write!(out, "Thinking…\r\n");
    if harness == Harness::Claude {
        let border = "─".repeat(columns.saturating_sub(1));
        let _ = write!(out, "{border}\r\n{content}\r\n{border}\r\n{footer}");
    } else {
        let _ = write!(out, "\x1b[48;2;63;69;77m\x1b[2K\r\n");
        for line in content.split("\r\n") {
            let _ = write!(out, "\x1b[2K{line}\r\n");
        }
        let _ = write!(out, "\x1b[2K\x1b[0m\r\n{footer}");
    }
    let _ = write!(out, "\x1b[0m\x1b[3;3H");
}

fn render(root: &Path, harness: Harness, state: &str, body: &str) {
    let mut out = std::io::stdout().lock();
    let prompt = harness.prompt();
    let draft = fs::read_to_string(root.join("draft.txt"))
        .unwrap_or_else(|_| "user draft second line".into());
    let visible = body.replace('\n', "\r\n");
    let _ = write!(out, "\x1b[?2004h\x1b[2J\x1b[H");
    match state {
        "approval" => {
            let _ = write!(
                out,
                "Do you want to proceed?\r\n{prompt} 1. Yes\r\n  2. No\x1b[2;3H"
            );
        }
        "question" => {
            let _ = write!(
                out,
                "Which option should be used?\r\n{prompt} 1. First\r\n  2. Second\x1b[2;3H"
            );
        }
        "draft" => {
            let _ = write!(out, "{prompt} user draft");
        }
        "draft-multiline" => {
            let _ = write!(out, "{prompt} \r\n{draft}\x1b[1;3H");
        }
        "attachment" => {
            let _ = write!(out, "[Image #1]\r\n{prompt} ");
        }
        "attachment-below" => {
            let _ = write!(out, "{prompt} \r\n[Attachment image.png]\x1b[1;3H");
        }
        "placeholder-draft" => {
            let _ = write!(out, "{prompt} [Pasted text #1]");
        }
        "history-placeholder" => {
            let _ = write!(out,"[Pasted text #1]\r\nprevious output\r\nprevious output\r\nprevious output\r\n{prompt} {visible}");
        }
        "history-only" if !body.is_empty() => {
            let _ = write!(out, "{visible}\r\n\r\n{prompt} ");
        }
        "paste-placeholder" if !body.is_empty() => {
            let _ = write!(
                out,
                "{prompt} [Pasted text #1 +{} lines]",
                body.lines().count()
            );
        }
        "busy" => {
            let _ = write!(out, "Thinking…\r\n{prompt} {visible}");
        }
        "busy-footer" => {
            input_box(&mut out, harness, &format!("{prompt} {visible}"));
        }
        "footer-draft" => {
            let footer = if harness == Harness::Claude {
                "bypass permissions on (shift+tab to cycle)"
            } else {
                "gpt-5.5 medium · fixture · main · Ask for approval · Context 3% used"
            };
            input_box(&mut out, harness, &format!("{prompt} \r\n{footer}"));
        }
        "typed-placeholder" => {
            input_box(
                &mut out,
                harness,
                &format!("{prompt} Run /review on my current changes"),
            );
        }
        "styled-placeholder" if body.is_empty() => {
            input_box(
                &mut out,
                harness,
                &format!("{prompt} \x1b[2;90mRun /review on my current changes\x1b[22;39m"),
            );
        }
        "styled-placeholder" => {
            input_box(&mut out, harness, &format!("{prompt} {visible}"));
        }
        _ => {
            let _ = write!(out, "{prompt} {visible}");
        }
    }
    let _ = out.flush();
    write_json(
        root,
        "rendered.json",
        &format!(
            "{{\"state\":{},\"body\":{},\"at_ms\":{}}}",
            escaped(state),
            escaped(body),
            now_ms()
        ),
    );
}

fn write_user(transcript: &mut File, harness: Harness, sid: &str, body: &str) {
    if harness == Harness::Codex {
        writeln!(transcript,"{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":{}}}]}}}}",escaped(body)).unwrap();
    } else {
        writeln!(transcript,"{{\"type\":\"user\",\"sessionId\":{},\"message\":{{\"role\":\"user\",\"content\":{}}}}}",escaped(sid),escaped(body)).unwrap();
    }
    transcript.sync_all().unwrap();
}

enum Input {
    Paste(Vec<u8>),
    Enter,
    Other,
}

fn next_input(buffer: &mut Vec<u8>) -> Option<Input> {
    if buffer.is_empty() {
        return None;
    }
    if PASTE_START.starts_with(buffer) {
        return None;
    }
    if buffer.starts_with(PASTE_START) {
        let end = buffer[PASTE_START.len()..]
            .windows(PASTE_END.len())
            .position(|part| part == PASTE_END)?
            + PASTE_START.len();
        let body = buffer[PASTE_START.len()..end].to_vec();
        buffer.drain(..end + PASTE_END.len());
        Some(Input::Paste(body))
    } else {
        let byte = buffer.remove(0);
        Some(if byte == b'\r' {
            Input::Enter
        } else {
            Input::Other
        })
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let argument = |name: &str| {
        args.windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
            .expect("fixture argument missing")
    };
    let sid = argument("--session-id");
    assert!(
        sid.len() == 36
            && sid
                .bytes()
                .enumerate()
                .all(|(i, b)| if [8, 13, 18, 23].contains(&i) {
                    b == b'-'
                } else {
                    b.is_ascii_hexdigit()
                })
    );
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let harness = match executable.file_name().and_then(|name| name.to_str()) {
        Some("claude") => Harness::Claude,
        Some("codex") => Harness::Codex,
        _ => panic!("fixture executable must be named claude or codex"),
    };
    let root = PathBuf::from(argument("--fixture-dir"))
        .canonicalize()
        .unwrap();
    assert!(
        root.is_dir() && root.join("ALLOW_TELL_FIXTURE").is_file(),
        "isolated fixture marker required"
    );
    let temporary = [
        "/tmp",
        "/private/tmp",
        "/var/folders",
        "/private/var/folders",
    ]
    .iter()
    .any(|base| root.starts_with(base));
    let isolated = root.ancestors().find(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("kasaterm-board-"))
    });
    assert!(
        temporary && isolated.is_some_and(|path| executable.starts_with(path)),
        "fixture and executable must share a temporary kasaterm-board-* root"
    );
    let transcript_path = root.join(if harness == Harness::Codex {
        format!("rollout-2026-09-16T00-00-00-{sid}.jsonl")
    } else {
        format!("{sid}.jsonl")
    });
    let mut transcript = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&transcript_path)
        .unwrap();
    if harness == Harness::Codex {
        // A live root rollout FD supplies the same identity evidence as the real process.
        writeln!(transcript,"{{\"type\":\"session_meta\",\"payload\":{{\"id\":{},\"session_id\":{},\"source\":\"cli\",\"originator\":\"tell-fixture\",\"cwd\":{}}}}}",escaped(&sid),escaped(&sid),escaped(&root.to_string_lossy())).unwrap();
    }
    write_user(&mut transcript, harness, &sid, "isolated tell fixture");
    let mut input = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("input.bin"))
        .unwrap();
    let mut events = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("events.jsonl"))
        .unwrap();
    assert!(
        std::process::Command::new("/bin/stty")
            .args(["raw", "-echo"])
            .status()
            .unwrap()
            .success(),
        "fixture requires a real PTY"
    );
    write_json(
        &root,
        "ready.json",
        &format!(
            "{{\"pid\":{},\"harness\":{},\"session_id\":{},\"transcript\":{}}}",
            std::process::id(),
            escaped(harness.name()),
            escaped(&sid),
            escaped(&transcript_path.to_string_lossy())
        ),
    );
    let (tx, rx) = mpsc::channel();
    let partial_exit = fs::read_to_string(root.join("state.txt"))
        .is_ok_and(|state| state.trim() == "quit-mid-paste");
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut bytes = [0u8; 4096];
        let limit = if partial_exit { 8 } else { bytes.len() };
        while let Ok(count) = stdin.read(&mut bytes[..limit]) {
            if count == 0 || tx.send(bytes[..count].to_vec()).is_err() {
                break;
            }
            // Leave unread PTY bytes for the parent shell instead of consuming them here.
            if partial_exit {
                break;
            }
        }
    });
    let mut state = String::new();
    let mut buffered = Vec::new();
    let mut body = String::new();
    let mut submits = 0;
    let mut pastes = 0;
    'fixture: loop {
        if root.join("exit.txt").is_file() {
            break;
        }
        let desired = fs::read_to_string(root.join("state.txt"))
            .unwrap_or_else(|_| "busy".into())
            .trim()
            .to_string();
        if desired != state {
            state = desired;
            render(&root, harness, &state, &body);
        }
        let bytes = match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(bytes) => bytes,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        input.write_all(&bytes).unwrap();
        input.sync_all().unwrap();
        if state == "quit-mid-paste" {
            break;
        }
        buffered.extend_from_slice(&bytes);
        while let Some(event) = next_input(&mut buffered) {
            match event {
                Input::Paste(bytes) => {
                    body = String::from_utf8(bytes).expect("fixture input must be UTF-8");
                    pastes += 1;
                    writeln!(
                        events,
                        "{{\"kind\":\"paste\",\"count\":{pastes},\"body\":{},\"at_ms\":{}}}",
                        escaped(&body),
                        now_ms()
                    )
                    .unwrap();
                    events.sync_all().unwrap();
                    write_json(
                        &root,
                        "pasted.json",
                        &format!("{{\"count\":{pastes},\"body\":{}}}", escaped(&body)),
                    );
                    if state == "quit-after-paste" {
                        break 'fixture;
                    }
                    render(&root, harness, &state, &body);
                }
                Input::Enter => {
                    submits += 1;
                    writeln!(
                        events,
                        "{{\"kind\":\"enter\",\"count\":{submits},\"body\":{},\"at_ms\":{}}}",
                        escaped(&body),
                        now_ms()
                    )
                    .unwrap();
                    events.sync_all().unwrap();
                    write_json(
                        &root,
                        "submitted.json",
                        &format!("{{\"count\":{submits},\"body\":{}}}", escaped(&body)),
                    );
                    write_user(&mut transcript, harness, &sid, &body);
                    body.clear();
                    render(&root, harness, &state, &body);
                }
                Input::Other => {}
            }
        }
    }
    write_json(
        &root,
        "exited.json",
        &format!(
            "{{\"pastes\":{pastes},\"enters\":{submits},\"state\":{}}}",
            escaped(&state)
        ),
    );
}
