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
//!   KASATERM_TMUX_TRACE   — log file path. Default
//!                           `${TMPDIR}/kasaterm-tmux-calls.log`
//!                           (Windows: `%TEMP%\kasaterm-tmux-calls.log`).
//!   KASATERM_REAL_TMUX    — explicit path to the real tmux binary. If
//!                           unset we scan a small list of common
//!                           install locations and skip ourselves.
//!
//! Exit code mirrors the underlying tmux (or 127 when no real tmux
//! is on disk).

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

fn default_trace_path() -> PathBuf {
    std::env::temp_dir().join("kasaterm-tmux-calls.log")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());

    let trace_path = std::env::var("KASATERM_TMUX_TRACE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_trace_path());
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
        let quoted: Vec<String> = args.iter().map(|a| format!("{a:?}")).collect();
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
        Some(path) => run_real_tmux(&path, &args),
        None => {
            eprintln!(
                "kasaterm tmux-shim: no real tmux on disk. Set \
                 KASATERM_REAL_TMUX to override, or install tmux."
            );
            std::process::exit(127);
        }
    }
}

#[cfg(unix)]
fn run_real_tmux(path: &PathBuf, args: &[String]) -> ! {
    // Unix: replace ourselves with the real tmux so the parent process
    // tree sees the real binary, not a shim wrapper.
    use std::os::unix::process::CommandExt;
    let err = Command::new(path).args(args).exec();
    eprintln!("kasaterm tmux-shim: exec {path:?} failed: {err}");
    std::process::exit(127);
}

#[cfg(windows)]
fn run_real_tmux(path: &PathBuf, args: &[String]) -> ! {
    // Windows has no exec replacement: the closest equivalent is to
    // spawn-and-wait, then forward the child's exit code. The shim
    // process stays alive while tmux runs, which is one extra wait()
    // in the parent — caller can't distinguish.
    let status = Command::new(path).args(args).status();
    match status {
        Ok(s) => std::process::exit(s.code().unwrap_or(127)),
        Err(e) => {
            eprintln!("kasaterm tmux-shim: spawn {path:?} failed: {e}");
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
            let pane = std::env::var("TMUX_PANE").unwrap_or_else(|_| "%0".to_string());
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
            let pane = std::env::var("TMUX_PANE").unwrap_or_else(|_| "%0".to_string());
            println!("0: [80x24] [history 0/2000, 0 bytes] {pane} (active)");
            Some(0)
        }
        // `tmux has-session [-t name]` — exit 0 means "yes it exists".
        "has-session" => Some(0),
        // Phase 2: route mutating commands to the kasaterm
        // agent-socket via the cmux-compat binary staged next to us.
        // `split-window`, `send-keys`, `select-pane` are the bare
        // minimum Claude Code's teammate-mode needs to spawn panes,
        // pump keystrokes, and bring a teammate to focus. Anything we
        // don't map yet logs and exits 0 so the caller keeps moving.
        "split-window" => Some(route_split_window(args)),
        "send-keys" => Some(route_send_keys(args)),
        "select-pane" => Some(route_select_pane(args)),
        "kill-pane" | "new-window" | "new-session" | "rename-window"
        | "set-environment" | "setenv" | "set-option" | "set" | "set-hook"
        | "show-environment" | "showenv" => {
            eprintln!("[tmux-shim] {head} accepted (stub, no kasaterm RPC yet)");
            Some(0)
        }
        _ => None,
    }
}

/// Find the cmux-compat binary alongside us so the shim can spawn it
/// without depending on PATH luck. install_tmux_shim drops it into the
/// same dir that holds the tmux shim symlink and exports
/// KASATERM_TMUX_SHIM_DIR.
fn cmux_compat_path() -> Option<PathBuf> {
    let dir = std::env::var("KASATERM_TMUX_SHIM_DIR").ok()?;
    let exe = if cfg!(windows) { "cmux-compat.exe" } else { "cmux-compat" };
    let p = PathBuf::from(dir).join(exe);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

fn run_cmux_compat(args: &[&str]) -> Result<i32, String> {
    let path = cmux_compat_path()
        .ok_or_else(|| "cmux-compat binary not found in KASATERM_TMUX_SHIM_DIR".to_string())?;
    let status = Command::new(&path)
        .args(args)
        .status()
        .map_err(|e| format!("spawn cmux-compat: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

/// Map `tmux split-window [-h|-v] [-t target] ...` to
/// `cmux-compat split <direction>`. tmux's -h means "horizontal split"
/// = side-by-side (cmux `right`); -v means stacked (cmux `down`).
/// Defaults to `down` to mirror tmux's own default for a flag-less
/// `split-window`.
fn route_split_window(args: &[String]) -> i32 {
    let args = if args.first().map(String::as_str) == Some("split-window") {
        &args[1..]
    } else {
        args
    };
    let direction = if args.iter().any(|a| a == "-h") {
        "right"
    } else if args.iter().any(|a| a == "-v") {
        "down"
    } else {
        "down"
    };
    match run_cmux_compat(&["split", direction]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("[tmux-shim] split-window route failed: {e}");
            1
        }
    }
}

/// Map `tmux send-keys [-t target] <key-or-text>...` to a sequence of
/// `cmux-compat send` / `key` calls. tmux send-keys is variadic: each
/// non-flag arg is either a literal string or a key name (Enter,
/// C-c, etc.). We forward each accordingly so a typical
/// `send-keys -t %1 "ls -al" C-m` becomes `send --surface %1 "ls -al"`
/// followed by `key enter`.
fn route_send_keys(args: &[String]) -> i32 {
    // args still includes the leading "send-keys" command name —
    // skip it so it isn't forwarded as a literal text payload.
    let args = if args.first().map(String::as_str) == Some("send-keys") {
        &args[1..]
    } else {
        args
    };
    let mut target: Option<String> = None;
    let mut payloads: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-t" => {
                if let Some(next) = args.get(i + 1) {
                    target = Some(next.clone());
                    i += 2;
                    continue;
                }
                i += 1;
            }
            // -l literal flag: tmux uses it to disable key-name parsing.
            // We always treat unrecognised tokens as text so the flag is
            // a no-op for us.
            "-l" | "-R" | "-X" | "-M" => {
                i += 1;
            }
            other => {
                payloads.push(other.to_string());
                i += 1;
            }
        }
    }
    for payload in &payloads {
        let mapped_key = map_tmux_key(payload);
        let result = if let Some(k) = mapped_key {
            let mut cmd: Vec<&str> = Vec::new();
            cmd.push("key");
            cmd.push(k);
            run_cmux_compat(&cmd)
        } else {
            let mut cmd: Vec<String> =
                vec!["send".to_string()];
            if let Some(t) = &target {
                cmd.push("--surface".to_string());
                cmd.push(t.clone());
            }
            cmd.push(payload.clone());
            let cmd_ref: Vec<&str> = cmd.iter().map(String::as_str).collect();
            run_cmux_compat(&cmd_ref)
        };
        if let Err(e) = result {
            eprintln!("[tmux-shim] send-keys forward {payload:?} failed: {e}");
            return 1;
        }
    }
    0
}

/// Translate tmux's key names into the cmux-compat `key` vocabulary.
/// Returns None when the argument is a literal text fragment (no
/// translation needed — caller forwards it as `send`).
fn map_tmux_key(s: &str) -> Option<&'static str> {
    match s {
        "Enter" | "C-m" | "C-M" => Some("enter"),
        "Tab" | "C-i" | "C-I" => Some("tab"),
        "Escape" | "C-[" => Some("escape"),
        "BSpace" | "Backspace" | "C-h" | "C-H" => Some("backspace"),
        "Delete" => Some("delete"),
        "Up" | "C-Up" => Some("up"),
        "Down" | "C-Down" => Some("down"),
        "Left" | "C-Left" => Some("left"),
        "Right" | "C-Right" => Some("right"),
        _ => None,
    }
}

/// `tmux select-pane -t <id>` → `cmux-compat focus <id>`. Tmux also
/// accepts -L/-R/-U/-D for directional focus; we don't have a
/// directional focus RPC yet, so log+0 those.
fn route_select_pane(args: &[String]) -> i32 {
    let args = if args.first().map(String::as_str) == Some("select-pane") {
        &args[1..]
    } else {
        args
    };
    let target = args
        .iter()
        .position(|a| a == "-t")
        .and_then(|i| args.get(i + 1));
    let Some(t) = target else {
        eprintln!("[tmux-shim] select-pane without -t: ignored");
        return 0;
    };
    match run_cmux_compat(&["focus", t]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("[tmux-shim] select-pane route failed: {e}");
            1
        }
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
    for c in real_tmux_candidates() {
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

#[cfg(unix)]
fn real_tmux_candidates() -> &'static [&'static str] {
    &[
        "/opt/homebrew/bin/tmux",
        "/usr/local/bin/tmux",
        "/usr/bin/tmux",
        "/opt/local/bin/tmux",
    ]
}

#[cfg(windows)]
fn real_tmux_candidates() -> &'static [&'static str] {
    // Windows has no canonical tmux install location and Windows-native
    // tmux does not exist. Caller can still point us at a WSL bridge or
    // a custom build via KASATERM_REAL_TMUX.
    &[]
}
