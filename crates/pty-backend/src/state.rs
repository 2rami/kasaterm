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

use alacritty_terminal::event::{Event as AlacEvent, EventListener};
use alacritty_terminal::index::Point;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::{Color as VtColor, NamedColor, Processor, StdSyncHandler};
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
}

impl Default for PtyOptions {
    fn default() -> Self {
        Self {
            shell: None,
            cwd: None,
            cols: 80,
            rows: 24,
            env: Vec::new(),
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

        // Spin up the VT processor loop. Owns the Term, drains the
        // reader, and emits a ScreenUpdate after each batch. Bounded
        // channel + drop-on-full keeps us from buffering frames the
        // renderer is too slow to consume.
        let reader_thread =
            spawn_reader_thread(reader, tx, opts.cols, opts.rows, size.clone());

        Ok(Self {
            screens: rx,
            master,
            writer: Arc::new(Mutex::new(writer)),
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

/// Alacritty Term's `EventListener` slot is mostly bell / title /
/// resize callbacks. We don't need most of them — title arrives via
/// the existing OSC channel that we already pull out of the cell
/// snapshot, bells we ignore for now. Empty impl is fine.
#[derive(Clone, Default)]
struct NoopListener;
impl EventListener for NoopListener {
    fn send_event(&self, _ev: AlacEvent) {}
}

/// Local Dimensions impl. alacritty_terminal exposes the trait but
/// the concrete TermSize we want to pass lives behind a "test"
/// feature gate in some versions; this keeps us decoupled.
fn make_term(cols: u16, rows: u16) -> Term<NoopListener> {
    let size = TermSize::new(cols as usize, rows as usize);
    Term::new(TermConfig::default(), &size, NoopListener)
}

fn spawn_reader_thread(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<ScreenUpdate>,
    cols: u16,
    rows: u16,
    size: Arc<Mutex<(u16, u16)>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut processor: Processor<StdSyncHandler> = Processor::new();
        let mut term = make_term(cols, rows);
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
                Ok(0) => return, // EOF — shell exited
                Ok(n) => n,
                Err(e) => {
                    eprintln!("[pty-backend] read error: {e}");
                    return;
                }
            };
            processor.advance(&mut term, &buf[..n]);
            // Snapshot Term → ScreenUpdate. We emit every visible row
            // as dirty for now — alacritty_terminal tracks per-line
            // dirty flags internally but exposing them needs a deeper
            // adaptation; pushing the full grid keeps the renderer
            // happy and the per-row Arc-identity cache in cells.rs
            // still catches no-op rows.
            let update = snapshot(&term, current_size.0, current_size.1);
            if tx.send(update).is_err() {
                return;
            }
        }
    })
}

fn snapshot(term: &Term<NoopListener>, cols: u16, rows: u16) -> ScreenUpdate {
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
        pane_id: "%0".to_string(),
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
