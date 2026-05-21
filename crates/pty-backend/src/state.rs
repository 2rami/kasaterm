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
        cmd.env("TERM", "xterm-256color");
        // Masquerade as iTerm.app so child TUIs (Claude Code, helix,
        // etc.) treat us as a capability-rich terminal and emit OSC 52
        // for copy-on-select. Our alacritty parser handles OSC 52
        // correctly; iTerm-specific escapes that we don't support are
        // either silently dropped or render as small visual noise — the
        // tradeoff favors working clipboard integration.
        cmd.env("TERM_PROGRAM", "iTerm.app");
        cmd.env("TERM_PROGRAM_VERSION", "3.5.0");
        cmd.env("COLORTERM", "truecolor");
        for k in [
            "ITERM_SESSION_ID",
            "ITERM_PROFILE",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            // Ghostty / WezTerm / Alacritty leave their own crumbs too —
            // strip them so a TUI can't mis-attribute us to whichever
            // emulator happened to spawn the parent shell.
            "GHOSTTY_RESOURCES_DIR",
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
        if let Ok(fake_tmux) = std::env::var("KASATERM_TMUX_SHIM_TMUX") {
            cmd.env("TMUX", fake_tmux);
        }
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
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        let offset = t.grid().display_offset();
        let update = snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
        let _ = self.screens_tx.try_send(update);
        offset
    }

    /// Jump straight to the live tail (display offset 0).
    pub fn scroll_to_bottom(&self) {
        let (cols, rows) = *self.size.lock().unwrap();
        let mut t = self.term.lock().unwrap();
        t.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        let update = snapshot(&mut t, cols, rows, &self.pane_id, &self.title_handle, true);
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
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
        }
    }
}

impl EventListener for PtyEventForwarder {
    fn send_event(&self, event: AlacEvent) {
        match event {
            AlacEvent::PtyWrite(s) => self.write_to_pty(s.as_bytes()),
            AlacEvent::ColorRequest(_, formatter) => {
                // Until we propagate a real palette, claim pure black
                // for any indexed-color query so the shell stops
                // blocking. Foreground/background detection that
                // depends on this will be wrong, but cmd / bash never
                // gate startup on the answer.
                let reply = formatter(Rgb { r: 0, g: 0, b: 0 });
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
        let mut buf = [0u8; 8192];
        let mut current_size = (cols, rows);
        // Reassembles UTF-8 across read boundaries so NFC normalization
        // never sees a half codepoint (a multibyte char split between
        // two reads).
        let mut utf8_buf = Utf8Buffer::new();

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
                    return;
                }
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[pty-backend] read error: {e}");
                    return;
                }
            };
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
            // precomposed syllables before alacritty stores them — the
            // GPU cell-renderer draws one glyph per codepoint, so NFD
            // would otherwise show as split jamo (cosmic_text used to
            // reshape them on the sugarloaf path).
            let raw_str = utf8_buf.process(&buf[..n]);
            let nfc_str: String = raw_str.nfc().collect();
            let processed_bytes = nfc_str.as_bytes();

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
                    let snap = snapshot(
                        &mut t,
                        current_size.0,
                        current_size.1,
                        &pane_id,
                        &title_handle,
                        false,
                    );
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
                if tx.send(upd).is_err() {
                    return;
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
    }
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
