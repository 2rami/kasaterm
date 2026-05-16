//! `tmux` shim binary that sits ahead of the real tmux on PATH when a
//! shell is spawned by kasaterm. Phase 0 of the cmux-style teammate
//! integration plan:
//!
//!   1. log every invocation with argv + cwd so we know which tmux
//!      commands Claude Code's teammateMode (and the `tmux-pane-job`
//!      skill) actually fire,
//!   2. exec the real tmux underneath so behavior is unchanged while
//!      we collect data,
//!   3. expose env hooks the next phase will use to translate calls
//!      into RPC against KASATERM_SOCKET_PATH instead of delegating.
//!
//! Env contract:
//!   KASATERM_TMUX_TRACE   — log file path. Default `/tmp/kasaterm-tmux-calls.log`.
//!   KASATERM_REAL_TMUX    — explicit path to the real tmux binary. If
//!                           unset we scan a small list of common
//!                           install locations and skip ourselves.
//!
//! Exit code mirrors the underlying tmux (or 127 when no real tmux
//! is on disk).

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_TRACE: &str = "/tmp/kasaterm-tmux-calls.log";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());

    let trace_path = std::env::var("KASATERM_TMUX_TRACE")
        .unwrap_or_else(|_| DEFAULT_TRACE.to_string());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&trace_path)
    {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        // Quote argv elements so the log is unambiguous when commands
        // carry spaces / newlines / shell metacharacters.
        let quoted: Vec<String> = args
            .iter()
            .map(|a| format!("{a:?}"))
            .collect();
        let _ = writeln!(
            f,
            "{ts:.3}\tpid={}\tcwd={cwd}\ttmux {}",
            std::process::id(),
            quoted.join(" ")
        );
    }

    // Phase 1 minimum: serve a handful of read-only queries directly
    // so child tools that need "am I in tmux?" answers don't see real
    // tmux fail on our fake socket. Anything we don't recognize still
    // delegates to the real tmux below, where it's likely to fail —
    // log so we can grow this match each round.
    if let Some(code) = handle_known(&args) {
        std::process::exit(code);
    }

    let real = find_real_tmux();
    match real {
        Some(path) => {
            let err = Command::new(&path).args(&args).exec();
            eprintln!("kasaterm tmux-shim: exec {path:?} failed: {err}");
            std::process::exit(127);
        }
        None => {
            eprintln!(
                "kasaterm tmux-shim: no real tmux on disk. Set \
                 KASATERM_REAL_TMUX to override, or install tmux."
            );
            std::process::exit(127);
        }
    }
}

/// Hardcoded responses for the slice of tmux we actually need to keep
/// Claude Code's teammateMode happy. Returns Some(exit_code) when the
/// arg list matched a known shape; None to fall through to real tmux.
///
/// This is intentionally NOT the full tmux protocol — every match here
/// is something we observed Claude Code asking for. Add cases as the
/// trace log grows; do not pre-emptively cover the whole tmux surface.
fn handle_known(args: &[String]) -> Option<i32> {
    let head = args.first().map(String::as_str)?;
    match head {
        // `tmux display-message -p <fmt>` — synchronous getter. Claude
        // Code uses it to fish out the current pane / session ids.
        "display-message" => {
            let fmt = args
                .iter()
                .position(|a| a == "-p")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
                .unwrap_or("");
            let pane = std::env::var("TMUX_PANE")
                .unwrap_or_else(|_| "%0".to_string());
            let out = match fmt {
                "#{pane_id}" => pane.clone(),
                "#{session_name}" => "kasaterm".to_string(),
                "#{session_id}" => "$0".to_string(),
                "#{window_id}" => "@0".to_string(),
                "#S:#W.#P" => format!("kasaterm:0.{}", strip_pct(&pane)),
                "#S" => "kasaterm".to_string(),
                "#{pane_pid}" => std::process::id().to_string(),
                other if other.is_empty() => pane.clone(),
                // Unknown format — print empty rather than blow up.
                _ => String::new(),
            };
            println!("{out}");
            Some(0)
        }
        // `tmux list-sessions` — Claude Code surveys sessions before
        // deciding whether to spawn a new one.
        "list-sessions" | "ls" => {
            println!("kasaterm: 1 windows (created now) (attached)");
            Some(0)
        }
        // `tmux list-windows`, `tmux list-panes` — basic structure.
        "list-windows" | "lsw" => {
            println!("0: kasaterm* (1 panes)");
            Some(0)
        }
        "list-panes" | "lsp" => {
            let pane = std::env::var("TMUX_PANE")
                .unwrap_or_else(|_| "%0".to_string());
            println!("0: [80x24] [history 0/2000, 0 bytes] {pane} (active)");
            Some(0)
        }
        // `tmux has-session [-t name]` — exit 0 means "yes it exists".
        "has-session" => Some(0),
        // Mutating commands. These would have to translate to the
        // kasaterm RPC to actually take effect — Phase 1 is just
        // "don't fail loudly so the caller can proceed". We log via
        // the same trace path so we know they were requested.
        "split-window" | "send-keys" | "kill-pane" | "select-pane"
        | "new-window" | "new-session" | "rename-window"
        | "set-environment" | "setenv" | "set-option" | "set"
        | "set-hook" | "show-environment" | "showenv" => {
            // For now, pretend success. Phase 2 wires these to the
            // kasaterm socket so they actually move panes.
            eprintln!(
                "[tmux-shim] {head} accepted (stub, no kasaterm RPC yet)"
            );
            Some(0)
        }
        _ => None,
    }
}

fn strip_pct(s: &str) -> &str {
    s.strip_prefix('%').unwrap_or(s)
}

fn find_real_tmux() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_REAL_TMUX") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    // PATH may still point at us (we just claimed the `tmux` name), so
    // skip our own binary when scanning. argv[0] gives the shim path
    // when invoked via PATH.
    let self_path = std::env::current_exe().ok();
    let candidates = [
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
        "/usr/bin/tmux",
        "/opt/local/bin/tmux",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if !p.is_file() {
            continue;
        }
        if self_path.as_ref().is_some_and(|s| s == &p) {
            continue;
        }
        return Some(p);
    }
    None
}
