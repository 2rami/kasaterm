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
    // tmux supports several global options before the command itself,
    // e.g. `tmux -S <socket> display-message ...` or `tmux -L <name>
    // -2 has-session`. Strip them so the match below sees the command
    // verb regardless of how the caller framed it.
    let args = strip_global_options(args);
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
            // claude code calls `list-panes -t @0 -F #{pane_id}` to
            // enumerate live panes. Query the real workspace via
            // cmux-compat and print one surface id per line; fall back
            // to the canned mock output for any other format string.
            let format = args
                .iter()
                .position(|a| a == "-F")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);
            if matches!(format, Some("#{pane_id}")) {
                if let Some(path) = cmux_compat_path() {
                    if let Ok(out) = Command::new(&path).args(["list", "surfaces"]).output() {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        for id in extract_all_surface_ids(&stdout) {
                            println!("{id}");
                        }
                        return Some(0);
                    }
                }
                // cmux-compat unreachable — still emit the focused pane
                // so a single-pane caller doesn't get an empty list.
                let pane =
                    std::env::var("TMUX_PANE").unwrap_or_else(|_| "%0".to_string());
                println!("{pane}");
                Some(0)
            } else {
                let pane = std::env::var("TMUX_PANE").unwrap_or_else(|_| "%0".to_string());
                println!("0: [80x24] [history 0/2000, 0 bytes] {pane} (active)");
                Some(0)
            }
        }
        // `tmux has-session [-t name]` — exit 0 means "yes it exists".
        "has-session" => Some(0),
        // Phase 2: route mutating commands to the kasaterm
        // agent-socket via the cmux-compat binary staged next to us.
        // `split-window`, `send-keys`, `select-pane` are the bare
        // minimum Claude Code's teammate-mode needs to spawn panes,
        // pump keystrokes, and bring a teammate to focus. Anything we
        // don't map yet logs and exits 0 so the caller keeps moving.
        "split-window" => Some(route_split_window(&args)),
        "send-keys" => Some(route_send_keys(&args)),
        "select-pane" => Some(route_select_pane(&args)),
        "kill-pane" => Some(route_kill_pane(&args)),
        "swap-pane" => Some(route_swap_pane(&args)),
        "new-window" | "new-session" | "rename-window"
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
    let exe = if cfg!(windows) { "cmux-compat.exe" } else { "cmux-compat" };
    // Primary lookup: env exported by install_tmux_shim.
    if let Ok(dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
        let p = PathBuf::from(dir).join(exe);
        if p.is_file() {
            return Some(p);
        }
    }
    // Fallback: cmux-compat staged next to us in the same dir. Covers
    // standalone invocations where the env var isn't set.
    if let Ok(self_exe) = std::env::current_exe() {
        if let Some(parent) = self_exe.parent() {
            let p = parent.join(exe);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn run_cmux_compat(args: &[&str]) -> Result<i32, String> {
    let path = cmux_compat_path()
        .ok_or_else(|| "cmux-compat binary not found in KASATERM_TMUX_SHIM_DIR".to_string())?;
    let mut cmd = Command::new(&path);
    cmd.args(args);
    // Explicitly forward the socket env so cmux-compat doesn't fall
    // back to the platform default (`\\.\pipe\cmux` on Windows) when
    // a parent shell forgets — or refuses — to leak it on its own.
    if let Ok(sock) = std::env::var("KASATERM_SOCKET_PATH") {
        cmd.env("KASATERM_SOCKET_PATH", sock);
    }
    if let Ok(sock) = std::env::var("CMUX_SOCKET_PATH") {
        cmd.env("CMUX_SOCKET_PATH", sock);
    }
    let status = cmd.status().map_err(|e| format!("spawn cmux-compat: {e}"))?;
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
    // tmux `-P` flag asks for the new pane's info to be printed; the
    // `-F #{pane_id}` format claude code's teammate-mode uses wants the
    // pane id alone. cmux-compat speaks JSON, so capture stdout, fish
    // out `"id":"%N"` from the response, and emit just that. Without
    // this the JSON object ends up substituted into every follow-up
    // `-t ...` target.
    let print_pane_id = args.iter().any(|a| a == "-P");
    let path = match cmux_compat_path() {
        Some(p) => p,
        None => {
            eprintln!("[tmux-shim] split-window: cmux-compat binary not found");
            return 1;
        }
    };
    let output = match Command::new(&path).args(["split", direction]).output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("[tmux-shim] split-window spawn failed: {e}");
            return 1;
        }
    };
    if print_pane_id {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(id) = extract_surface_id(&stdout) {
            println!("{id}");
        } else {
            // Fall back to echoing raw stdout when we can't find an id —
            // worst case the caller fails the same way it did before.
            print!("{stdout}");
        }
    } else {
        // No -P: tmux is silent on success; mirror that.
    }
    output.status.code().unwrap_or(1)
}

/// Pull a `"id":"%N"` value out of a cmux-compat JSON response.
/// Lightweight by design — we only need this one shape so a real
/// serde_json dependency isn't worth the build-time hit on the shim.
fn extract_surface_id(s: &str) -> Option<String> {
    // Look for `"surface":{...,"id":"%N",...}` first to avoid matching
    // the outer `"id":"cli-…"` request id.
    let surface_pos = s.find(r#""surface""#)?;
    let after = &s[surface_pos..];
    let id_pos = after.find(r#""id":""#)?;
    let rest = &after[id_pos + r#""id":""#.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Pull every `%N`-shaped id out of a cmux-compat `list surfaces`
/// response. Mirrors `extract_surface_id`'s pragmatic string-search
/// instead of pulling in serde_json.
fn extract_all_surface_ids(s: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find(r#""id":""#) {
        let after = &rest[pos + r#""id":""#.len()..];
        match after.find('"') {
            Some(end) => {
                let id = &after[..end];
                if id.starts_with('%') {
                    ids.push(id.to_string());
                }
                rest = &after[end..];
            }
            None => break,
        }
    }
    ids
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
    // cmd.exe doesn't unwrap single-quote pairs, so a tmux caller's
    // `send-keys -t %1 'claude --print "..."' Enter` shows up here as
    // a bunch of pre-split text fragments interleaved with the Enter
    // key. Rejoin consecutive text tokens into a single payload, then
    // strip a matching outer pair of single quotes — that recovers
    // the caller's original keystroke string when the original quoting
    // got chewed up by cmd.exe.
    let mut groups: Vec<SendGroup> = Vec::new();
    let mut current_text: Vec<String> = Vec::new();
    for payload in &payloads {
        if let Some(k) = map_tmux_key(payload) {
            if !current_text.is_empty() {
                groups.push(SendGroup::Text(unwrap_outer_single_quotes(
                    &current_text.join(" "),
                )));
                current_text.clear();
            }
            groups.push(SendGroup::Key(k));
        } else {
            current_text.push(payload.clone());
        }
    }
    if !current_text.is_empty() {
        groups.push(SendGroup::Text(unwrap_outer_single_quotes(
            &current_text.join(" "),
        )));
    }
    for group in groups {
        let result = match group {
            SendGroup::Key(k) => run_cmux_compat(&["key", k]),
            SendGroup::Text(text) => {
                let mut cmd: Vec<String> = vec!["send".to_string()];
                if let Some(t) = &target {
                    cmd.push("--surface".to_string());
                    cmd.push(t.clone());
                }
                cmd.push(text);
                let cmd_ref: Vec<&str> =
                    cmd.iter().map(String::as_str).collect();
                run_cmux_compat(&cmd_ref)
            }
        };
        if let Err(e) = result {
            eprintln!("[tmux-shim] send-keys forward failed: {e}");
            return 1;
        }
    }
    0
}

enum SendGroup {
    Text(String),
    Key(&'static str),
}

/// Strip a single matching pair of outer single quotes — and the
/// resulting `'\''` shell-escape glue between them — so a Unix-quoted
/// blob from `cd 'C:\path' && cmd` survives a round trip through
/// cmd.exe's quote-blind arg parser.
fn unwrap_outer_single_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        let inner = &trimmed[1..trimmed.len() - 1];
        // tmux callers use `'\''` to embed a single quote inside a
        // single-quoted string. Recover that to a bare `'`.
        return inner.replace("'\\''", "'");
    }
    s.to_string()
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
    // `select-pane -T <title>` renames the targeted pane (claude
    // teammate sets the agent name this way). Route it to rename.
    if let Some(title) = args
        .iter()
        .position(|a| a == "-T")
        .and_then(|i| args.get(i + 1))
    {
        if let Some(t) = target {
            return match run_cmux_compat(&["rename", t, title]) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("[tmux-shim] select-pane -T route failed: {e}");
                    1
                }
            };
        }
    }
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

/// Map `tmux swap-pane -s <src> -t <dst>` to `cmux-compat swap <src>
/// <dst>` so panes can trade places through the socket.
fn route_swap_pane(args: &[String]) -> i32 {
    let args = if args.first().map(String::as_str) == Some("swap-pane") {
        &args[1..]
    } else {
        args
    };
    let src = args
        .iter()
        .position(|a| a == "-s")
        .and_then(|i| args.get(i + 1));
    let dst = args
        .iter()
        .position(|a| a == "-t")
        .and_then(|i| args.get(i + 1));
    match (src, dst) {
        (Some(s), Some(d)) => match run_cmux_compat(&["swap", s, d]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("[tmux-shim] swap-pane route failed: {e}");
                1
            }
        },
        _ => {
            eprintln!("[tmux-shim] swap-pane needs -s and -t: ignored");
            0
        }
    }
}

/// Map `tmux kill-pane -t <target>` to `cmux-compat close <target>` so
/// a teammate (or any shell) can tear down a pane through the socket.
fn route_kill_pane(args: &[String]) -> i32 {
    let args = if args.first().map(String::as_str) == Some("kill-pane") {
        &args[1..]
    } else {
        args
    };
    let target = args
        .iter()
        .position(|a| a == "-t")
        .and_then(|i| args.get(i + 1));
    let Some(t) = target else {
        eprintln!("[tmux-shim] kill-pane without -t: ignored");
        return 0;
    };
    match run_cmux_compat(&["close", t]) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("[tmux-shim] kill-pane route failed: {e}");
            1
        }
    }
}

fn strip_pct(s: &str) -> &str {
    s.strip_prefix('%').unwrap_or(s)
}

/// Drop tmux's pre-command global options so `handle_known` can match
/// on the actual command verb. Mirrors `tmux(1) OPTIONS`:
///   -S socket-path / -L socket-name / -f config-file / -T features
///     all take a separate argument.
///   -2 -C -CC -D -l -N -q -u -v -V are flag-only.
///   `--` ends option processing.
fn strip_global_options(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    let mut stripping = true;
    while i < args.len() {
        let a = &args[i];
        if stripping {
            if a == "--" {
                stripping = false;
                i += 1;
                continue;
            }
            if matches!(a.as_str(), "-S" | "-L" | "-f" | "-T") {
                // consume option + its value
                i += 2;
                continue;
            }
            if matches!(
                a.as_str(),
                "-2" | "-C" | "-CC" | "-D" | "-l" | "-N" | "-q" | "-u" | "-v" | "-V"
            ) {
                i += 1;
                continue;
            }
            // First non-flag token is the command verb; stop stripping
            // from here so per-command flags (e.g. `display-message -t
            // %0`) stay intact.
            stripping = false;
        }
        out.push(a.clone());
        i += 1;
    }
    out
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
