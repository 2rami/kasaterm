//! `PtySession` — owns the PTY pair, the child shell process, the
//! alacritty_terminal VT state, and the threads that pump bytes in and
//! diffs out.
//!
//! Lifecycle: `PtySession::start(opts)` spawns the shell, kicks off a
//! reader thread that feeds bytes through `alacritty_terminal::Term`,
//! and exposes:
//!   - `screens: Receiver<ScreenUpdate>` — diffs the renderer consumes
//!   - `send_bytes(&[u8])` — write to the PTY (key input, paste, etc)
//!   - `resize(cols, rows)` — propagate window resize to the PTY +
//!     reshape the VT grid
//!
//! ScreenUpdate format matches tmux-bridge's so the renderer is happy
//! with either backend.

use std::time::Instant;

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::index::Point;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, TermDamage};
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor, Processor, Rgb, StdSyncHandler};
use alacritty_terminal::Term;
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tmux_bridge::screen::{Cell, Color, Row, ScreenUpdate};

/// What to spawn in the PTY. Sticks close to portable-pty's
/// CommandBuilder so the user can override env / cwd without us
/// re-implementing a shell-spawn API.
#[derive(Debug, Clone)]
pub struct PtyOptions {
    pub shell: Option<String>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub env: Vec<(String, String)>,
    /// Identifier this session stamps on every ScreenUpdate it emits.
    /// The renderer keys panes by this id, so a multi-pane workspace
    /// gives each PtySession a unique value ("%0", "%1", ...).
    pub pane_id: String,
    /// Scrollback to seed on start (oldest→newest text lines). Fed through the
    /// VT parser before the shell's first output so it lands in alacritty's
    /// scrollback and shows on scroll-up. Empty = fresh terminal. Restores a
    /// pane's pre-restart screen content across a relaunch.
    pub initial_scrollback: Vec<String>,
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            shell: None,
            cwd: None,
            cols: 80,
            rows: 24,
            env: Vec::new(),
            pane_id: "%0".to_string(),
            initial_scrollback: Vec::new(),
        }
    }
}

pub struct PtySession {
    /// Channel the renderer consumes — one ScreenUpdate per dirty
    /// frame after VT processing landed new state.
    pub screens: Receiver<ScreenUpdate>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    /// Shared cell-dim state used by the resize path so we can reshape
    /// the VT grid without re-creating the Term.
    size: Arc<Mutex<(u16, u16)>>,
    /// Held so the renderer thread doesn't get GC'd; never read from
    /// after start().
    _reader_thread: std::thread::JoinHandle<()>,
    /// PID of the shell we spawned. We walk the process tree from
    /// here to find the active foreground command (vim, claude, etc.)
    /// so the pane header can label itself the way iTerm does — by
    /// running process rather than by OSC title.
    shell_pid: Option<u32>,
    /// (last_query_at, cached_name). Throttle the ps(1) shellout to
    /// ~500ms so a 60Hz render loop doesn't fork-exec on every frame.
    proc_cache: Arc<Mutex<(Instant, Option<String>)>>,
    /// Shared Term so `scroll()` can drive alacritty's own scrollback
    /// (display_offset) from the main thread and re-snapshot. Using
    /// alacritty's scrollback — instead of a hand-rolled shift
    /// detection — is what makes scroll-region TUIs (claude code's
    /// pinned input) scroll back correctly.
    term: Arc<Mutex<Term<PtyEventForwarder>>>,
    /// tx clone so `scroll()` can push the re-snapshot to the same
    /// channel the reader thread feeds.
    screens_tx: Sender<ScreenUpdate>,
    title_handle: Arc<Mutex<Option<String>>>,
    pane_id: String,
}

impl PtySession {
    pub fn start(opts: PtyOptions) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: opts.rows,
                cols: opts.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;
        // Default to the user's login shell. CommandBuilder picks up
        // $SHELL fallback on its own when we don't override; pass `-il`
        // when we know we're handing off to zsh / bash so .zshrc /
        // .bashrc gets sourced (matches what tmux-bridge does inside
        // its `new-session -d 'exec $SHELL -il'`).
        let mut cmd = if let Some(shell) = opts.shell.as_deref() {
            let mut c = CommandBuilder::new(shell);
            c.arg("-il");
            c
        } else {
            // Use the default shell from $SHELL.
            CommandBuilder::new_default_prog()
        };
        if let Some(cwd) = opts.cwd.as_deref() {
            cmd.cwd(cwd);
        }
        // Terminal-identity env. portable-pty's CommandBuilder inherits
        // the parent process env, so if we were launched from iTerm /
        // Ghostty / Terminal.app, child TUIs (Claude Code, vim, etc)
        // see `TERM_PROGRAM=iTerm.app` and treat us as that host —
        // sending iTerm-only escapes that our alacritty parser would
        // either ignore or render as garbage. Force a consistent
        // identity and scrub the iTerm-specific leftovers so the
        // detection settles on kasaterm regardless of who launched us.
        // TERM defaults to xterm-256color. If the ghostty terminfo entry
        // is installed on this machine (ghostty ships it in its app
        // bundle), upgrade to TERM=xterm-ghostty + point TERMINFO at the
        // ghostty terminfo dir — Claude Code / vim / less then read the
        // richer capability set (SGR mouse, ccc/oc colour redefinition,
        // title set/clear, repeat-character `rep`, undercurl, etc) and
        // emit the wider variety of styles the user sees in real ghostty.
        // Fallback path keeps working unchanged on machines without it.
        let ghostty_terminfo_dir = "/Applications/Ghostty.app/Contents/Resources/terminfo";
        let ghostty_terminfo_entry =
            format!("{ghostty_terminfo_dir}/78/xterm-ghostty");
        let has_ghostty_terminfo = std::path::Path::new(&ghostty_terminfo_entry).exists();
        if has_ghostty_terminfo {
            cmd.env("TERM", "xterm-ghostty");
            cmd.env("TERMINFO", ghostty_terminfo_dir);
        } else {
            cmd.env("TERM", "xterm-256color");
        }
        // Masquerade as Ghostty. Claude Code (and other TUIs) detects
        // host terminal via `TERM_PROGRAM` and adapts its theme — the
        // Ghostty profile uses the punchier diff bg / syntax colours
        // the user wanted. OSC 52 clipboard still works; alacritty's
        // VT parser handles whatever escapes Ghostty-targeted TUIs
        // emit (sync output, kitty graphics, etc).
        cmd.env("TERM_PROGRAM", "ghostty");
        // Match the actual ghostty version installed on this machine so
        // Claude Code / Helix / etc don't disable capabilities they think
        // we're too old for. Version-gated feature checks are a common
        // pattern; the 1.1.3 placeholder we shipped earlier predated
        // truecolor-default behaviour in some clients.
        cmd.env("TERM_PROGRAM_VERSION", "1.3.1");
        cmd.env("COLORTERM", "truecolor");
        // No FORCE_COLOR — ghostty doesn't set it (verified by running
        // `node -e 'console.log(process.env.FORCE_COLOR)'` inside ghostty,
        // which returns `undefined`). Some color-detection libraries treat
        // a present FORCE_COLOR as evidence the program is running under
        // CI / a wrapper that's already stripped tty info, and downgrade
        // to ANSI-256 instead of trusting COLORTERM=truecolor. Mirroring
        // ghostty's environment exactly avoids that branch.
        // Some clients double-check "is this a real ghostty" by looking
        // for GHOSTTY_BIN_DIR (set by ghostty itself when it spawns a
        // shell). Without it they fall back to a conservative capability
        // set and emit ANSI-256 instead of truecolor.
        cmd.env(
            "GHOSTTY_BIN_DIR",
            "/Applications/Ghostty.app/Contents/MacOS",
        );
        for k in [
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            // WezTerm / Alacritty leave their own crumbs too — strip them
            // so a TUI can't mis-attribute us. GHOSTTY_RESOURCES_DIR is
            // NOT in this list anymore because we want to set it
            // ourselves below; portable-pty's `env_remove` wipes the
            // entry from the same BTreeMap we just inserted into, so
            // including it here would silently undo our `env` call.
            "WEZTERM_PANE",
            "WEZTERM_EXECUTABLE",
            "ALACRITTY_LOG",
            "ALACRITTY_WINDOW_ID",
        ] {
            cmd.env_remove(k);
        }
        // tmux-shim hook. When kasaterm has set up a shim directory
        // and exported KASATERM_TMUX_SHIM_DIR + KASATERM_TMUX_SHIM_TMUX
        // before spawning us, prepend it to PATH and fake $TMUX so any
        // tmux call inside this PTY hits our shim instead of the real
        // binary. That gives the next phase a chance to translate
        // `tmux split-window` etc into BSP RPC calls.
        if let Ok(shim_dir) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            let parent_path = std::env::var("PATH").unwrap_or_default();
            // PATH separator is platform-specific: `:` on Unix,
            // `;` on Windows. Using `:` on Windows folds the whole
            // chain into one literal entry and breaks every lookup.
            let sep = if cfg!(windows) { ';' } else { ':' };
            cmd.env("PATH", format!("{shim_dir}{sep}{parent_path}"));
            // Point zsh at the shim dir's rc files (written by
            // install_tmux_shim). They source the user's real rc first,
            // then re-prepend the shim dir to PATH — so it survives
            // brew's zprofile prepend and `tmux` resolves to our shim.
            // zsh-only; other shells ignore ZDOTDIR and use the PATH
            // prepend above.
            cmd.env("ZDOTDIR", &shim_dir);
        }
        // Don't set TMUX in the child env. Claude Code / ink / chalk
        // treat the presence of TMUX as "I'm inside tmux, which doesn't
        // pass truecolor through by default" and downgrade their
        // ANSI escape generation to 256-palette mode. Verified
        // empirically: removing TMUX is the single change that makes
        // claude code emit `\e[48;2;220;38;39m` to us instead of the
        // quantised `\e[48;5;167m`. The KASATERM_TMUX_SHIM_TMUX env
        // (a path to our shim socket) is still inherited as itself,
        // so any tool that explicitly reads it for the shim still works;
        // we just don't masquerade as a tmux-wrapped shell.
        let _ = std::env::var("KASATERM_TMUX_SHIM_TMUX"); // intentionally not propagated as TMUX
        // Real tmux sets TMUX_PANE on every child so an `if [ -n
        // "$TMUX_PANE" ]` test passes inside a pane. Claude Code's
        // teammateMode reads this to know which pane it's currently
        // running in — without it, the `display-message -p` subprocess
        // never gets called and we get "Could not determine current
        // tmux pane/window" before our shim sees anything.
        cmd.env("TMUX_PANE", &opts.pane_id);
        // Cross-pane RPC: each pane needs to know (a) which surface it
        // is and (b) where to reach the host so a script inside one
        // pane can drive another via cmux-compat. CommandBuilder
        // inherits the parent env by default, but make these two
        // explicit so removing the inherit later doesn't silently
        // break the integration.
        cmd.env("KASATERM_PANE_ID", &opts.pane_id);
        if let Ok(sock) = std::env::var("KASATERM_SOCKET_PATH") {
            cmd.env("KASATERM_SOCKET_PATH", sock);
        }
        // Caller-supplied env overrides everything above so tests /
        // callers can still inject a synthetic TERM if they need to.
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("spawn shell into PTY")?;
        let shell_pid = child.process_id();
        // We drop the slave half — the spawned child holds the only
        // fd we care about. Keeping it open in our process makes
        // close-detection unreliable.
        drop(pair.slave);
        // Master knows the slave's tty path (e.g. /dev/ttys011) — Terminal.app
        // shows this as the trailing "on ttysNNN" of its Last login line and
        // we want to mirror that. Only available on unix; None on Windows.
        #[cfg(unix)]
        let tty_short = pair
            .master
            .tty_name()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
        #[cfg(not(unix))]
        let tty_short: Option<String> = None;

        let reader = pair.master.try_clone_reader().context("clone reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("take writer")?;
        let master = Arc::new(Mutex::new(pair.master));

        let (tx, rx) = bounded::<ScreenUpdate>(256);
        let size = Arc::new(Mutex::new((opts.cols, opts.rows)));
        let writer_arc: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(writer));

        // Spin up the VT processor loop. Owns the Term, drains the
        // reader, and emits a ScreenUpdate after each batch. Bounded
        // channel + drop-on-full keeps us from buffering frames the
        // renderer is too slow to consume.
        let title_handle = Arc::new(Mutex::new(None));
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer_arc),
            size: Arc::clone(&size),
            last_title: Arc::clone(&title_handle),
        };
        let term = Arc::new(Mutex::new(make_term(opts.cols, opts.rows, listener)));
        // Seed restored scrollback into alacritty before the shell's first
        // output, so scroll-up shows the pre-restart screen content. Fed as if
        // it were program output (v1: plain text, no color/attrs).
        if !opts.initial_scrollback.is_empty() {
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let mut t = term.lock().unwrap();
            for line in &opts.initial_scrollback {
                proc.advance(&mut *t, line.as_bytes());
                proc.advance(&mut *t, b"\r\n");
            }
        }
        // Mimic Terminal.app's "Last login: …" banner. login(1) writes this
        // by reading ~/.lastlogin and updating it after spawn; we keep our
        // own state file (no setuid login wrapper involved) and inject the
        // line straight into the VT grid before the reader thread starts —
        // same pattern as initial_scrollback above. We only show it when a
        // previous timestamp exists, so a brand-new install doesn't get a
        // bare "Last login: on ttysNNN" line.
        if let Some(line) = build_last_login_line(tty_short.as_deref()) {
            let mut proc: Processor<StdSyncHandler> = Processor::new();
            let mut t = term.lock().unwrap();
            proc.advance(&mut *t, line.as_bytes());
            proc.advance(&mut *t, b"\r\n");
        }
        let reader_thread = spawn_reader_thread(
            reader,
            tx.clone(),
            opts.cols,
            opts.rows,
            size.clone(),
            opts.pane_id.clone(),
            Arc::clone(&title_handle),
            Arc::clone(&term),
        );

        Ok(Self {
            screens: rx,
            master,
            writer: writer_arc,
            _child: Arc::new(Mutex::new(child)),
            size,
            _reader_thread: reader_thread,
            shell_pid,
            proc_cache: Arc::new(Mutex::new((
                Instant::now() - std::time::Duration::from_secs(1),
                None,
            ))),
            term,
            screens_tx: tx,
            title_handle,
            pane_id: opts.pane_id.clone(),
        })
    }

    /// Best-effort label for what's running in this PTY *right now*.
    /// Returns the comm name of the most recently spawned child of
    /// our shell (typically the foreground command — vim, claude,
    /// less, …) or falls back to the shell's own comm. ps(1) is
    /// throttled to ~500ms so this is cheap to call from the render
    /// loop.
    /// The shell's process id (None if it failed to launch). Used to look up
    /// the active pane's cwd for the git panel.
    pub fn shell_pid(&self) -> Option<u32> {
        self.shell_pid
    }

    pub fn active_process_name(&self) -> Option<String> {
        let pid = self.shell_pid?;
        let now = Instant::now();
        let mut cache = self.proc_cache.lock().ok()?;
        if now.duration_since(cache.0).as_millis() < 500 {
            return cache.1.clone();
        }
        cache.0 = now;
        let output = std::process::Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,comm="])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&output.stdout);
        let mut best_child: Option<(u32, String)> = None;
        let mut shell_comm: Option<String> = None;
        for line in s.lines() {
            let mut parts = line.split_whitespace();
            let row_pid = parts.next().and_then(|s| s.parse::<u32>().ok())?;
            let row_ppid = parts.next().and_then(|s| s.parse::<u32>().ok())?;
            let comm = parts.collect::<Vec<_>>().join(" ");
            if row_pid == pid {
                let name = std::path::Path::new(&comm)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&comm)
                    .to_string();
                shell_comm = Some(name);
            } else if row_ppid == pid {
                let name = std::path::Path::new(&comm)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&comm)
                    .to_string();
                if best_child.as_ref().is_none_or(|(p, _)| *p < row_pid) {
                    best_child = Some((row_pid, name));
                }
            }
        }
        let resolved = best_child.map(|(_, n)| n).or(shell_comm);
        cache.1 = resolved.clone();
        resolved
    }

    pub fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes).context("pty write")?;
        // Flush immediately. Without this, a one-shot write that isn't
        // followed by another (a committed Hangul syllable — the next
        // keystroke only updates the preedit overlay, not the PTY) sits
        // in the writer buffer until something else flushes it, so the
        // shell echoes "안" ~0.2s late and the user sees only the preedit
        // "ㄴ" until then. ASCII typing hid this because each keystroke's
        // write flushed the previous one.
        w.flush().context("pty flush")?;
        Ok(())
    }

    /// Scroll the view through alacritty's scrollback by `lines`
    /// (positive = toward older history / up, negative = toward the
    /// live tail / down). Re-snapshots immediately and pushes the
    /// frame so the renderer reflects the new position without waiting
    /// for PTY output — important for an idle TUI like claude. Returns
    /// the resulting display offset (0 = at the live bottom).
    pub fn scroll(&self, lines: i32) -> usize {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        let before = t.grid().display_offset();
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        let after = t.grid().display_offset();
        // Inertia at a scrollback boundary keeps firing scroll(±N) even
        // though the offset is clamped. Skipping the snapshot+send when
        // nothing moved lets the render thread answer a direction reverse
        // immediately instead of working through a queue of no-ops.
        if before == after {
            return after;
        }
        let update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        let _ = self.screens_tx.try_send(update);
        after
    }

    /// Jump straight to the live tail (display offset 0).
    pub fn scroll_to_bottom(&self) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        let update = snapshot(
            &mut t,
            cols,
            rows,
            &self.pane_id,
            &self.title_handle,
            true,
        );
        let _ = self.screens_tx.try_send(update);
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        // Propagate the new size into both the kernel-side PTY (so the
        // child sees SIGWINCH) and our local Term state (so cell
        // indices stay in range and the next reader pass reshapes).
        let pty = self.master.lock().unwrap();
        pty.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("pty resize")?;
        let mut size = self.size.lock().unwrap();
        *size = (cols, rows);
        Ok(())
    }
}

/// Bridges alacritty_terminal's `EventListener` callbacks back into
/// the PTY's input side. This is non-optional: terminals expect the
/// host to *reply* to a handful of control sequences, not just
/// passively render them. Without this, `\e[6n` (DSR-CPR) issued by
/// the shell on startup blocks waiting for a cursor-position report
/// and ConPTY-attached cmd.exe never reaches its first prompt.
///
/// We translate the events that carry a wire-format payload into
/// writes against the PTY master:
///   - PtyWrite — raw bytes alacritty already formatted
///   - ColorRequest — RGB query; reply with a fixed default
///   - TextAreaSizeRequest — geometry query; reply with current grid
///   - ClipboardLoad — paste request; reply with empty until we wire
///     real OS clipboard access through arboard
///
/// MouseCursorDirty / Title / Bell / etc are pure UI signals; the
/// renderer reads title/cursor state from the snapshot, so we drop
/// them here.
struct PtyEventForwarder {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    size: Arc<Mutex<(u16, u16)>>,
    /// Latest OSC 0 / OSC 2 title pushed by the shell or any TUI
    /// running inside it. `None` after `ResetTitle` or until the
    /// first set. The reader thread reads this on each snapshot so
    /// the renderer's pane-header strip can reflect "✱ Claude Code",
    /// "vim filename", current cwd, etc. — anything the inner
    /// program decides to advertise.
    last_title: Arc<Mutex<Option<String>>>,
}

impl Clone for PtyEventForwarder {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            size: Arc::clone(&self.size),
            last_title: Arc::clone(&self.last_title),
        }
    }
}

impl PtyEventForwarder {
    fn write_to_pty(&self, bytes: &[u8]) {
        // Mirror outgoing replies into KASATERM_PTY_OUT_LOG so the
        // ghostty-vs-us escape diff can include OUR side of the
        // conversation (OSC 11 colour replies, TextAreaSize replies,
        // clipboard responses, etc) — not just what claude code sends.
        if let Ok(path) = std::env::var("KASATERM_PTY_OUT_LOG") {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write;
                let preview: String = bytes
                    .iter()
                    .take(2048)
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                let _ = writeln!(file, "[out] {} bytes: {}", bytes.len(), preview);
            }
        }
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
        }
    }
}

impl EventListener for PtyEventForwarder {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(s) => self.write_to_pty(s.as_bytes()),
            AlacEvent::ColorRequest(index, formatter) => {
                // Reply with values that match ghostty's defaults so
                // that Claude Code / other TUIs which probe the host
                // via OSC 10 (fg), OSC 11 (bg), OSC 12 (cursor), or
                // OSC 4;N (palette) make the same theme decisions
                // they make under ghostty. The old "black for
                // everything" answer caused Claude Code to treat us
                // as a near-black or unknown background and pick a
                // muted red palette (215,95,95 source instead of
                // ghostty's 220,38,39).
                //
                // NamedColor::Foreground = 256, Background = 257,
                // Cursor = 258 (per vte::ansi::NamedColor); 0-15 are
                // the ANSI base palette, 16-255 the xterm cube + gray
                // ramp.
                let rgb = match index {
                    // ANSI 16-base palette, ghostty default theme.
                    0 => (0x1D, 0x1F, 0x21),
                    1 => (0xCC, 0x66, 0x66),
                    2 => (0xB5, 0xBD, 0x68),
                    3 => (0xF0, 0xC6, 0x74),
                    4 => (0x81, 0xA2, 0xBE),
                    5 => (0xB2, 0x94, 0xBB),
                    6 => (0x8A, 0xBE, 0xB7),
                    7 => (0xC5, 0xC8, 0xC6),
                    8 => (0x66, 0x66, 0x66),
                    9 => (0xD5, 0x4E, 0x53),
                    10 => (0xB9, 0xCA, 0x4A),
                    11 => (0xE7, 0xC5, 0x47),
                    12 => (0x7A, 0xA6, 0xDA),
                    13 => (0xC3, 0x97, 0xD8),
                    14 => (0x70, 0xC0, 0xB1),
                    15 => (0xEA, 0xEA, 0xEA),
                    // 256 = NamedColor::Foreground. Match ghostty's
                    // bright white fg (#C5C8C6, the same as palette[7]).
                    256 => (0xC5, 0xC8, 0xC6),
                    // 257 = NamedColor::Background. Match ghostty's
                    // default dark bg #1D1F21. Claude Code uses this
                    // to decide light/dark theme and the exact red
                    // shade for diff highlights.
                    257 => (0x1D, 0x1F, 0x21),
                    // 258 = NamedColor::Cursor. Match palette[7].
                    258 => (0xC5, 0xC8, 0xC6),
                    // 16-255: xterm 6×6×6 cube + 24-step gray ramp,
                    // identical to ghostty's hardcoded fallback.
                    n if n >= 16 && n < 232 => {
                        let n = n - 16;
                        let steps = [0u8, 95, 135, 175, 215, 255];
                        (steps[n / 36], steps[(n / 6) % 6], steps[n % 6])
                    }
                    n if n >= 232 && n < 256 => {
                        let v = 8 + ((n - 232) as u8) * 10;
                        (v, v, v)
                    }
                    // Any other index (dim variants, etc): fall back
                    // to a sensible neutral grey.
                    _ => (0x66, 0x66, 0x66),
                };
                let reply = formatter(Rgb { r: rgb.0, g: rgb.1, b: rgb.2 });
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::TextAreaSizeRequest(formatter) => {
                let (cols, rows) = *self.size.lock().unwrap();
                let reply = formatter(WindowSize {
                    num_lines: rows,
                    num_cols: cols,
                    cell_width: 7,
                    cell_height: 16,
                });
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardLoad(_, formatter) => {
                // Read the OS clipboard and feed it back. Falls back
                // to empty so a clipboard-open failure doesn't strand
                // the shell waiting on a paste response.
                let text = arboard::Clipboard::new()
                    .ok()
                    .and_then(|mut cb| cb.get_text().ok())
                    .unwrap_or_default();
                let reply = formatter(&text);
                self.write_to_pty(reply.as_bytes());
            }
            AlacEvent::ClipboardStore(_, text) => {
                // OSC 52 set — Claude Code, helix, etc. push selected
                // text into the host clipboard through this. Best-
                // effort: a clipboard open failure is logged but does
                // not break the PTY.
                let preview: String = text.chars().take(40).collect();
                eprintln!(
                    "[pty-backend] OSC 52 set ({} chars): {preview:?}",
                    text.len()
                );
                match arboard::Clipboard::new() {
                    Ok(mut cb) => {
                        if let Err(e) = cb.set_text(text) {
                            eprintln!("[pty-backend] clipboard set failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("[pty-backend] clipboard open failed: {e}"),
                }
            }
            AlacEvent::Title(name) => {
                eprintln!("[pty-backend] OSC title set: {name:?}");
                if let Ok(mut t) = self.last_title.lock() {
                    *t = Some(name);
                }
            }
            AlacEvent::ResetTitle => {
                if let Ok(mut t) = self.last_title.lock() {
                    *t = None;
                }
            }
            // UI hints with no PTY-side reply.
            AlacEvent::MouseCursorDirty
            | AlacEvent::CursorBlinkingChange
            | AlacEvent::Wakeup
            | AlacEvent::Bell
            | AlacEvent::Exit
            | AlacEvent::ChildExit(_) => {}
        }
    }
}

/// Local Dimensions impl. alacritty_terminal exposes the trait but
/// the concrete TermSize we want to pass lives behind a "test"
/// feature gate in some versions; this keeps us decoupled.
fn make_term(cols: u16, rows: u16, listener: PtyEventForwarder) -> Term<PtyEventForwarder> {
    let size = TermSize::new(cols as usize, rows as usize);
    Term::new(TermConfig::default(), &size, listener)
}

#[allow(clippy::too_many_arguments)]
fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<ScreenUpdate>,
    cols: u16,
    rows: u16,
    size: Arc<Mutex<(u16, u16)>>,
    pane_id: String,
    title_handle: Arc<Mutex<Option<String>>>,
    term: Arc<Mutex<Term<PtyEventForwarder>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        use unicode_normalization::UnicodeNormalization;
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        // 64KB matches the macOS PTY kernel buffer — one read drains a full
        // frame's worth of TUI output (ghostty / iTerm sized buffer). The
        // old 8KB forced 2-3 reads per claude-code frame, multiplying the
        // per-read snapshot cost by 2-3× and capping throughput at ~90 fps.
        let mut buf = [0u8; 65536];
        let mut current_size = (cols, rows);
        // Raw byte trace for diagnosing capability-detection differences
        // between us and ghostty. Set `KASATERM_PTY_LOG=/tmp/pty.log` and
        // each read appends `[pane_id] hex bytes\n` to that file. Open it
        // only when the env var is present so production runs pay nothing.
        let pty_log = std::env::var("KASATERM_PTY_LOG").ok().and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .ok()
        });
        let pty_log = std::sync::Mutex::new(pty_log);
        // Reassembles UTF-8 across read boundaries so NFC normalization
        // never sees a half codepoint (a multibyte char split between
        // two reads).
        let mut utf8_buf = Utf8Buffer::new();
        // OSC 1337 inline-image capture state (a payload can span reads).
        let mut img_buf: Vec<u8> = Vec::new();
        let mut img_capturing = false;
        // Kitty graphics protocol (APC `\x1b_G…\x1b\\`) capture state. One
        // image can arrive as multiple APC chunks linked by `m=1` / `m=0` —
        // `kitty_chunk_buf` is the current chunk's raw bytes, while
        // `kitty_payload_buf` accumulates the decoded body across chunks.
        let mut kitty_chunk_buf: Vec<u8> = Vec::new();
        let mut kitty_payload_buf: Vec<u8> = Vec::new();
        let mut kitty_capturing = false;

        loop {
            // Check for a pending resize before we read more bytes —
            // a half-processed frame at the old size would land cells
            // out of bounds otherwise.
            let want = *size.lock().unwrap();
            if want != current_size {
                let s = TermSize::new(want.0 as usize, want.1 as usize);
                term.lock().unwrap().resize(s);
                current_size = want;
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    eprintln!("[pty-backend] EOF on PTY reader — shell exited");
                    // Tell the host pump the pane died. The PtySession also
                    // holds a Sender (for scroll/resize), so dropping our
                    // clone alone never closes the channel — the recv loop
                    // would block forever and the pane would linger as a
                    // zombie. An explicit eof sentinel reaps it instead.
                    let _ = tx.send(ScreenUpdate {
                        pane_id: pane_id.clone(),
                        eof: true,
                        ..Default::default()
                    });
                    return;
                }
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[pty-backend] read error: {e}");
                    let _ = tx.send(ScreenUpdate {
                        pane_id: pane_id.clone(),
                        eof: true,
                        ..Default::default()
                    });
                    return;
                }
            };
            // Append raw bytes (hex + escaped-printable preview) to the
            // KASATERM_PTY_LOG file so claude-code escape sequences can
            // be diffed against ghostty's `script` capture.
            if let Some(file) = pty_log.lock().unwrap().as_mut() {
                use std::io::Write;
                let preview: String = buf[..n.min(2048)]
                    .iter()
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                let _ = writeln!(file, "[{}] {} bytes: {}", pane_id, n, preview);
            }
            if std::env::var("KASATERM_LOG_PTY").is_ok() {
                let preview: String = buf[..n.min(2048)]
                    .iter()
                    .map(|b| match b {
                        0x20..=0x7e => (*b as char).to_string(),
                        b'\n' => "\\n".to_string(),
                        b'\r' => "\\r".to_string(),
                        b'\t' => "\\t".to_string(),
                        0x1b => "\\e".to_string(),
                        _ => format!("\\x{b:02x}"),
                    })
                    .collect();
                eprintln!("[pty-backend] read {n} bytes: {preview}");
            }

            // NFC-normalize so decomposed Hangul (NFD jamo) collapses to
            // precomposed syllables before alacritty stores them. Pure-ASCII
            // batches (the common case — TUI rendering, ANSI control flow)
            // skip the normalize entirely; NFC is a no-op there but the
            // .nfc() iterator + String alloc still cost ~10us per read in
            // a hot loop. ASCII fast-path keeps the bytes borrowed.
            let raw_str = utf8_buf.process(&buf[..n]);
            let (nfc_holder, processed_bytes): (Option<String>, &[u8]) =
                if raw_str.is_ascii() {
                    (None, raw_str.as_bytes())
                } else {
                    let s: String = raw_str.nfc().collect();
                    (Some(s), &[])
                };
            let processed_bytes: &[u8] = match &nfc_holder {
                Some(s) => s.as_bytes(),
                None => processed_bytes,
            };
            // Sniff for iTerm OSC 1337 inline images / kitty graphics. Both
            // scans walk the byte slice, so we cheaply prefix-check first —
            // most reads have no `\x1b]1337` / `\x1b_G` and we skip the
            // walk entirely. Critical for TUI throughput (claude code emits
            // thousands of small reads per second with neither prefix).
            if img_capturing
                || memchr::memmem::find(processed_bytes, b"\x1b]1337").is_some()
            {
                scan_inline_image(processed_bytes, &mut img_buf, &mut img_capturing);
            }
            if kitty_capturing
                || memchr::memmem::find(processed_bytes, b"\x1b_G").is_some()
            {
                scan_kitty_graphics(
                    processed_bytes,
                    &mut kitty_chunk_buf,
                    &mut kitty_payload_buf,
                    &mut kitty_capturing,
                );
            }

            let update = {
                let mut t = term.lock().unwrap();
                processor.advance(&mut *t, processed_bytes);
                // alacritty buffers DECSET 2026 synchronized output internally:
                // while its sync buffer is non-empty the Term grid still holds
                // the pre-sync frame, so skip the snapshot until it flushes on
                // ?2026l or the sync timeout — no torn frame ever reaches us.
                if processor.sync_bytes_count() > 0 {
                    None
                } else {
                    // New PTY output snaps the view back to the live tail
                    // (display_offset = 0) — matches every terminal's
                    // "jump to bottom on output" behaviour and keeps the
                    // cursor row valid.
                    t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                    let t_snap = std::time::Instant::now();
                    let mut snap = snapshot(
                        &mut t,
                        current_size.0,
                        current_size.1,
                        &pane_id,
                        &title_handle,
                        false,
                    );
                    // OSC 133 `B` = prompt end / command-input start. Our
                    // VT parser (alacritty 0.26 / vte 0.15) drops OSC 133
                    // as unhandled, so we sniff the raw batch for it and
                    // tag the snapshot with the current cursor — that's
                    // where the editable command line begins. The shell's
                    // precmd hook (injected via the ZDOTDIR shim .zshrc)
                    // is what emits it. Terminator-agnostic (BEL or ST).
                    if find_subslice(processed_bytes, b"\x1b]133;B").is_some() {
                        snap.prompt_end = Some((snap.cursor_row, snap.cursor_col));
                    }
                    if std::env::var_os("KASATERM_PROFILE").is_some() {
                        eprintln!(
                            "[snapshot] {}us {}x{} ({}b in)",
                            t_snap.elapsed().as_micros(),
                            current_size.0,
                            current_size.1,
                            n
                        );
                    }
                    Some(snap)
                }
            };
            if let Some(upd) = update {
                // try_send (not send) so the reader is NEVER paced by the
                // pump/render side. If the consumer is behind (slow GPU
                // pass, ws-lock contention) the bounded channel fills, the
                // newest snapshot gets dropped, and bash keeps writing at
                // full PTY rate. Reader produces a fresh snapshot on the
                // next read anyway, so the only cost is a momentary stale
                // frame — which the user wouldn't have seen mid-burst.
                // The blocking `send` previously stalled the reader, which
                // backpressured bash, which made claude-code-style TUIs
                // feel ~10× slower than ghostty.
                match tx.try_send(upd) {
                    Ok(()) => {}
                    Err(crossbeam_channel::TrySendError::Full(_)) => {}
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
                }
            }
        }
    })
}

/// Reassembles UTF-8 across PTY read boundaries. A read can split a
/// multibyte codepoint; buffering the tail until the next read keeps NFC
/// normalization from ever seeing a partial char.
struct Utf8Buffer {
    leftover: Vec<u8>,
}

impl Utf8Buffer {
    fn new() -> Self {
        Self { leftover: Vec::new() }
    }

    fn process(&mut self, data: &[u8]) -> String {
        self.leftover.extend_from_slice(data);
        let mut valid_up_to = 0;
        let mut i = 0;
        while i < self.leftover.len() {
            let b = self.leftover[i];
            let width = if b & 0x80 == 0 {
                1
            } else if b & 0xe0 == 0xc0 {
                2
            } else if b & 0xf0 == 0xe0 {
                3
            } else if b & 0xf8 == 0xf0 {
                4
            } else {
                1
            };
            if i + width <= self.leftover.len() {
                if std::str::from_utf8(&self.leftover[i..i + width]).is_ok() {
                    valid_up_to = i + width;
                }
                i += width;
            } else {
                break;
            }
        }
        if valid_up_to > 0 {
            let s = std::str::from_utf8(&self.leftover[..valid_up_to])
                .unwrap_or("")
                .to_string();
            self.leftover.drain(..valid_up_to);
            s
        } else {
            String::new()
        }
    }
}

fn snapshot(
    term: &mut Term<PtyEventForwarder>,
    cols: u16,
    rows: u16,
    pane_id: &str,
    last_title: &Arc<Mutex<Option<String>>>,
    // When false, only the lines alacritty marked damaged since the last
    // reset are rebuilt — a 1-char echo touches ~1 line instead of the
    // whole grid (180us → ~10us). The renderer keys ScreenUpdate.dirty by
    // row and leaves untouched rows alone, so a partial list is correct.
    // Callers that change the *whole* view (scroll, resize) pass true.
    force_full: bool,
) -> ScreenUpdate {
    // display_offset counts lines scrolled toward older history; visual
    // row r maps to grid line `r - display_offset`. Read it before the
    // &mut borrow from `damage()`.
    let display_offset = term.grid().display_offset() as i32;
    // Which visual rows to rebuild. damage() yields viewport-relative
    // line numbers (already display_offset-adjusted), and returns Full
    // on first frame / resize / scroll, which we expand to every row.
    let damaged: Vec<u16> = if force_full {
        (0..rows).collect()
    } else {
        match term.damage() {
            TermDamage::Full => (0..rows).collect(),
            TermDamage::Partial(iter) => {
                let mut v: Vec<u16> =
                    iter.map(|b| b.line as u16).filter(|&r| r < rows).collect();
                v.sort_unstable();
                v.dedup();
                v
            }
        }
    };
    term.reset_damage();
    let grid = term.grid();
    let mut dirty: Vec<(u16, Row)> = Vec::with_capacity(damaged.len());
    for &r in &damaged {
        let mut row: Row = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(r as i32 - display_offset),
                alacritty_terminal::index::Column(c as usize),
            );
            let cell = &grid[point];
            row.push(convert_cell(cell));
        }
        dirty.push((r, row));
    }
    let cursor = term.grid().cursor.point;
    let cursor_row = cursor.line.0.max(0) as u16;
    let cursor_col = cursor.column.0 as u16;
    let mode = term.mode();
    // Hide the cursor while scrolled into history — the live cursor
    // sits at the bottom of the active area, which isn't where the
    // user is looking, so drawing it over scrollback is misleading.
    let cursor_visible = display_offset == 0
        && mode.contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
    let alt_screen = mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let mouse_enabled = mode.contains(alacritty_terminal::term::TermMode::MOUSE_REPORT_CLICK)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_DRAG)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION);
    let mouse_sgr = mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE);
    // OSC 0 / OSC 2 title pushed by the inner program. Cached in the
    // forwarder so we can return the latest value on every snapshot
    // rather than draining alacritty's pending-title queue once and
    // losing it.
    let title: Option<String> = last_title.lock().ok().and_then(|t| t.clone());
    ScreenUpdate {
        pane_id: pane_id.to_string(),
        rows,
        cols,
        dirty,
        cursor_row,
        cursor_col,
        cursor_visible,
        alt_screen,
        mouse_enabled,
        mouse_sgr,
        title,
        eof: false,
        // Filled in by the reader thread when this batch carried an
        // OSC 133 `B` mark — snapshot() itself doesn't parse the stream.
        prompt_end: None,
    }
}

/// First index where `needle` occurs in `haystack`, or None. Tiny
/// linear scan — used only to sniff the short OSC 133 prompt marker out
/// of each PTY read batch.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Standard base64 decode (no external crate). Ignores non-alphabet bytes
/// (whitespace, `=` padding) so it tolerates wrapped iTerm payloads.
fn b64_decode(s: &[u8]) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// MVP inline-image: a completed OSC 1337 body is `params:base64`. Decode
/// the payload, write a temp PNG/JPEG, and hand it to the existing image
/// pane viewer via the kasaspace `/open-image` endpoint. (True cell-flow
/// inline rendering is a later stage; this gets `imgcat`-style output
/// showing in kasaterm now.)
fn emit_inline_image(body: &[u8]) {
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return;
    };
    let bytes = b64_decode(&body[colon + 1..]);
    if bytes.len() < 16 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-inline-{nanos}.png"));
    if std::fs::write(&tmp, &bytes).is_err() {
        return;
    }
    let port = std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".into());
    let url = format!("http://127.0.0.1:{port}/open-image");
    let _ = std::process::Command::new("curl")
        .args([
            "-s",
            "--get",
            "--data-urlencode",
            &format!("path={}", tmp.display()),
            &url,
        ])
        .status();
}

/// Capture an iTerm OSC 1337 inline-image sequence that may span several
/// PTY reads. `buf`/`capturing` persist across calls. Marker
/// `ESC ] 1337 ; File=` … terminator BEL or ST. alacritty parses the OSC
/// and drops it (unhandled), so the base64 never reaches the grid — we
/// sniff the raw batch in parallel to grab the payload.
fn scan_inline_image(bytes: &[u8], buf: &mut Vec<u8>, capturing: &mut bool) {
    const MARKER: &[u8] = b"\x1b]1337;File=";
    let mut data = bytes;
    loop {
        if *capturing {
            let bel = data.iter().position(|&b| b == 0x07);
            let st = find_subslice(data, b"\x1b\\");
            let end = match (bel, st) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            match end {
                Some(e) => {
                    buf.extend_from_slice(&data[..e]);
                    emit_inline_image(buf);
                    buf.clear();
                    *capturing = false;
                    let term_len = if data.get(e) == Some(&0x07) { 1 } else { 2 };
                    data = &data[(e + term_len).min(data.len())..];
                }
                None => {
                    // Guard against unbounded growth on a malformed stream.
                    if buf.len() < 8 * 1024 * 1024 {
                        buf.extend_from_slice(data);
                    } else {
                        buf.clear();
                        *capturing = false;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, MARKER) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + MARKER.len()..];
                }
                None => return,
            }
        }
    }
}

/// A completed kitty graphics payload (`f=100` PNG bytes already decoded). Same
/// path as the iTerm OSC 1337 emitter — write a temp file and ask the kasaspace
/// MCP to open it in an image pane.
fn emit_kitty_image(payload: &[u8]) {
    if payload.len() < 16 {
        return;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("kasaterm-kitty-{nanos}.png"));
    if std::fs::write(&tmp, payload).is_err() {
        return;
    }
    let port = std::env::var("KASASPACE_MCP_PORT").unwrap_or_else(|_| "8765".into());
    let url = format!("http://127.0.0.1:{port}/open-image");
    let _ = std::process::Command::new("curl")
        .args([
            "-s",
            "--get",
            "--data-urlencode",
            &format!("path={}", tmp.display()),
            &url,
        ])
        .status();
}

/// Capture a kitty graphics protocol sequence (APC `\x1b_G<params>;<body>\x1b\\`)
/// that may span multiple PTY reads AND multiple chunks (linked by `m=1` /
/// `m=0`). MVP: only `f=100` (PNG) direct-base64 payloads are accepted —
/// `f=32`/`f=24` raw RGB(A) are skipped because the image pane expects a
/// decodable container. alacritty's VT parser drops APCs, so we sniff in
/// parallel from the raw byte stream.
fn scan_kitty_graphics(
    bytes: &[u8],
    chunk_buf: &mut Vec<u8>,
    payload_buf: &mut Vec<u8>,
    capturing: &mut bool,
) {
    const APC_G: &[u8] = b"\x1b_G";
    const ST: &[u8] = b"\x1b\\";
    let mut data = bytes;
    loop {
        if *capturing {
            match find_subslice(data, ST) {
                Some(e) => {
                    chunk_buf.extend_from_slice(&data[..e]);
                    // Parse params (before ';') and body (after).
                    let sep = chunk_buf.iter().position(|&b| b == b';');
                    if let Some(sep_pos) = sep {
                        let (params, body) = chunk_buf.split_at(sep_pos);
                        let body = &body[1..]; // skip ';'
                        let params_s = std::str::from_utf8(params).unwrap_or("");
                        let mut more = false;
                        let mut format_png = true; // default if missing
                        for kv in params_s.split(',') {
                            let kv = kv.trim();
                            if let Some((k, v)) = kv.split_once('=') {
                                match k {
                                    "m" => more = v == "1",
                                    // First chunk carries `f=`; subsequent
                                    // chunks usually omit it.
                                    "f" if !v.is_empty() => format_png = v == "100",
                                    _ => {}
                                }
                            }
                        }
                        // Reject non-PNG formats once detected; clear state.
                        if !format_png {
                            payload_buf.clear();
                            chunk_buf.clear();
                            *capturing = false;
                            data = &data[(e + ST.len()).min(data.len())..];
                            continue;
                        }
                        let decoded = b64_decode(body);
                        payload_buf.extend_from_slice(&decoded);
                        if !more {
                            emit_kitty_image(payload_buf);
                            payload_buf.clear();
                        }
                    }
                    chunk_buf.clear();
                    *capturing = false;
                    data = &data[(e + ST.len()).min(data.len())..];
                }
                None => {
                    chunk_buf.extend_from_slice(data);
                    if chunk_buf.len() > 8 * 1024 * 1024 {
                        chunk_buf.clear();
                        payload_buf.clear();
                        *capturing = false;
                    }
                    return;
                }
            }
        } else {
            match find_subslice(data, APC_G) {
                Some(start) => {
                    *capturing = true;
                    data = &data[start + APC_G.len()..];
                }
                None => return,
            }
        }
    }
}

/// Build the "Last login: <time> on <tty>" banner Terminal.app shows.
/// Returns None on first ever spawn (no stored timestamp) or when we
/// couldn't resolve a tty name — both cases would render as an
/// awkward partial line.
///
/// State lives at `$HOME/.config/kasaterm/last_login` as one line of
/// pre-formatted text (e.g. "Tue May 26 13:05:54"). We re-emit the
/// *previous* contents and overwrite with `date(1)`-formatted "now"
/// so the next spawn sees this run's timestamp.
fn build_last_login_line(tty: Option<&str>) -> Option<String> {
    let tty = tty?;
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    let dir = home.join(".config").join("kasaterm");
    let path = dir.join("last_login");
    let previous = std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Shell out to date(1) — saves pulling chrono/time into the
    // workspace just for one strftime call. Format matches what
    // Terminal.app writes ("%a %b %e %H:%M:%S").
    let now = std::process::Command::new("date")
        .args(["+%a %b %e %H:%M:%S"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(now) = &now {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(&path, now);
    }
    previous.map(|p| format!("Last login: {p} on {tty}"))
}

fn convert_cell(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let ch = if cell.c == '\0' { ' ' } else { cell.c };
    Cell {
        ch: ch.to_string(),
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        bold: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::BOLD),
        italic: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::ITALIC),
        underline: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::UNDERLINE),
        inverse: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::INVERSE),
        dim: cell
            .flags
            .contains(alacritty_terminal::term::cell::Flags::DIM),
    }
}

fn convert_color(c: VtColor) -> Color {
    match c {
        VtColor::Named(NamedColor::Foreground) | VtColor::Named(NamedColor::Background) => {
            Color::Default
        }
        VtColor::Named(n) => Color::Idx(n as u8),
        VtColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        VtColor::Indexed(i) => Color::Idx(i),
    }
}
