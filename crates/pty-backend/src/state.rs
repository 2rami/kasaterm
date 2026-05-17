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

use alacritty_terminal::event::{Event as AlacEvent, EventListener, WindowSize};
use alacritty_terminal::index::Point;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::Config as TermConfig;
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
        cmd.env("TERM_PROGRAM", "kasaterm");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
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
        let listener = PtyEventForwarder {
            writer: Arc::clone(&writer_arc),
            size: Arc::clone(&size),
        };
        let reader_thread = spawn_reader_thread(
            reader,
            tx,
            opts.cols,
            opts.rows,
            size.clone(),
            opts.pane_id.clone(),
            listener,
        );

        Ok(Self {
            screens: rx,
            master,
            writer: writer_arc,
            _child: Arc::new(Mutex::new(child)),
            size,
            _reader_thread: reader_thread,
        })
    }

    pub fn send_bytes(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes).context("pty write")?;
        Ok(())
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
}

impl Clone for PtyEventForwarder {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
            size: Arc::clone(&self.size),
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
            // Title is exposed through the snapshot; the rest of the
            // events are UI hints with no PTY-side reply.
            AlacEvent::Title(_)
            | AlacEvent::ResetTitle
            | AlacEvent::MouseCursorDirty
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

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<ScreenUpdate>,
    cols: u16,
    rows: u16,
    size: Arc<Mutex<(u16, u16)>>,
    pane_id: String,
    listener: PtyEventForwarder,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        let mut term = make_term(cols, rows, listener);
        let mut buf = [0u8; 8192];
        let mut current_size = (cols, rows);
        loop {
            // Check for a pending resize before we read more bytes —
            // a half-processed frame at the old size would land cells
            // out of bounds otherwise.
            let want = *size.lock().unwrap();
            if want != current_size {
                let s = TermSize::new(want.0 as usize, want.1 as usize);
                term.resize(s);
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
            processor.advance(&mut term, &buf[..n]);
            // Snapshot Term → ScreenUpdate. We emit every visible row
            // as dirty for now — alacritty_terminal tracks per-line
            // dirty flags internally but exposing them needs a deeper
            // adaptation; pushing the full grid keeps the renderer
            // happy and the per-row Arc-identity cache in cells.rs
            // still catches no-op rows.
            let update = snapshot(&term, current_size.0, current_size.1, &pane_id);
            if tx.send(update).is_err() {
                return;
            }
        }
    })
}

fn snapshot(term: &Term<PtyEventForwarder>, cols: u16, rows: u16, pane_id: &str) -> ScreenUpdate {
    let grid = term.grid();
    let mut dirty: Vec<(u16, Row)> = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let mut row: Row = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            let point = Point::new(
                alacritty_terminal::index::Line(r as i32),
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
    let cursor_visible = mode.contains(alacritty_terminal::term::TermMode::SHOW_CURSOR);
    let alt_screen = mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
    let mouse_enabled = mode.contains(alacritty_terminal::term::TermMode::MOUSE_REPORT_CLICK)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_DRAG)
        || mode.contains(alacritty_terminal::term::TermMode::MOUSE_MOTION);
    let mouse_sgr = mode.contains(alacritty_terminal::term::TermMode::SGR_MOUSE);
    // Title arrives via `pop_title` which drains the pending OSC 0/2
    // queue. We don't pop — we'd lose the title on the next snapshot.
    // The renderer caches the last-applied title itself, so leaving
    // None here just means "no change this frame".
    let title: Option<String> = None;
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
