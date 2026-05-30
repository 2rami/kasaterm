//! kasaterm-native inbox: tail the kasa-chat mailbox and inject each new
//! message straight into the target pane's PTY, so an *idle* claude wakes up
//! and processes it as a fresh user turn. The harness Agent-Teams inbox only
//! wakes teammates the lead spawned via `Agent`; a hand-launched pane never
//! subscribes. Because we own the terminal we can do better — watch the file
//! ourselves and type the message in, exactly like KASATERM_AUTOSEND does.
//!
//! Reuses the existing `kasa-chat` log (`~/.kasaterm-chat/log.jsonl`) as the
//! mailbox so the sending interface (`kasa-chat send --to %N`) is unchanged —
//! this only adds the wake-capable delivery half. Renderer-free background
//! thread, modelled on `spawn_transcript_watcher`.
//!
//! Delivery is backend-specific (the daemon owns the PTYs and writes them
//! directly; the in-process backend queues a `SendBytes` for the main thread),
//! so the watcher takes two closures and stays agnostic about which is wired.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Duration;

/// Poll cadence — matches the transcript watcher. A note landing within ~0.75s
/// of being sent is plenty responsive for agent-to-agent chatter.
const TICK: Duration = Duration::from_millis(750);

/// Spawn the mailbox watcher on a background thread.
///
/// * `list_panes` — current live local pane ids (e.g. `["%0", "%1"]`).
/// * `inject` — write bytes to one pane's PTY (the wake mechanism).
pub fn spawn_inbox_watcher<L, I>(list_panes: L, inject: I)
where
    L: Fn() -> Vec<String> + Send + 'static,
    I: Fn(&str, &[u8]) + Send + 'static,
{
    if std::env::var("KASATERM_INBOX").as_deref() == Ok("0") {
        return;
    }
    let path = chat_log_path();
    std::thread::spawn(move || {
        // Start at EOF: we only deliver messages that arrive *after* launch,
        // never replay the backlog (which would spam every pane on startup).
        let mut offset: u64 = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        loop {
            std::thread::sleep(TICK);
            let Ok(mut f) = std::fs::File::open(&path) else {
                continue;
            };
            let len = f.metadata().map(|m| m.len()).unwrap_or(0);
            if len < offset {
                // File rotated/truncated — restart from the top.
                offset = 0;
            }
            if len == offset {
                continue;
            }
            if f.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }
            let mut buf = String::new();
            if f.read_to_string(&mut buf).is_err() {
                continue;
            }
            // Only advance past the last *complete* line, so a half-written
            // record is re-read whole on the next tick.
            let consumed = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
            offset += consumed as u64;
            for line in buf[..consumed].lines() {
                if let Some((from, to, text)) = parse_chat_line(line) {
                    deliver(&list_panes, &inject, &from, &to, &text);
                }
            }
        }
    });
}

/// Resolve the kasa-chat log path, honouring the same `KASATERM_CHAT_DIR`
/// override the shell script uses.
fn chat_log_path() -> PathBuf {
    if let Ok(dir) = std::env::var("KASATERM_CHAT_DIR") {
        return PathBuf::from(dir).join("log.jsonl");
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".kasaterm-chat").join("log.jsonl")
}

/// One mailbox record → (from, to, text). `to` is "" for a broadcast.
fn parse_chat_line(line: &str) -> Option<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let from = v.get("from")?.as_str()?.to_string();
    let to = v
        .get("to")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let text = v.get("text")?.as_str()?.to_string();
    Some((from, to, text))
}

/// Route one message to the right local pane(s) and inject it.
fn deliver<L, I>(list_panes: &L, inject: &I, from: &str, to: &str, text: &str)
where
    L: Fn() -> Vec<String>,
    I: Fn(&str, &[u8]),
{
    let panes = list_panes();

    // Directed note → that pane (if it's one of ours). Broadcast (to == "") →
    // every local pane except the sender, so a pane never echoes to itself.
    let targets: Vec<String> = if to.is_empty() {
        panes
            .iter()
            .filter(|id| id.as_str() != from)
            .cloned()
            .collect()
    } else if panes.iter().any(|id| id == to) {
        vec![to.to_string()]
    } else {
        Vec::new()
    };
    if targets.is_empty() {
        return;
    }

    // claude reads a bare '\n' as "submit", so any newline inside the body
    // would fire a partial turn — flatten to spaces and append exactly one.
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();

    for t in targets {
        let payload = format!("[쪽지 {from}] {flat}\n");
        inject(&t, payload.as_bytes());
    }
}
