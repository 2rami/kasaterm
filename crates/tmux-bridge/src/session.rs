//! tmux -C subprocess wrapper. Spawns reader + flusher threads, exposes
//! channels for events and screen diffs.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::event::{parse_line, TmuxEvent};
use crate::screen::{vt_cell, Cell, Row, ScreenUpdate};

type ParserMap = Arc<Mutex<HashMap<String, vt100::Parser>>>;

pub struct TmuxSession {
    child: Child,
    stdin: Mutex<ChildStdin>,
    parsers: ParserMap,
    /// (rows, cols) used to size newly-discovered pane parsers. Without
    /// this, vt100 defaults to 80×24 and writes from a 95-col app wrap
    /// at the wrong column, smearing characters across rows.
    pane_size: Arc<Mutex<(u16, u16)>>,
    /// Stored so Drop can run `tmux -L <socket> kill-server` against the
    /// same isolated socket we spawned on, without risking the user's
    /// default-socket sessions.
    socket_name: Option<String>,
    pub session_name: String,
    pub events: Receiver<TmuxEvent>,
    pub screens: Receiver<ScreenUpdate>,
    /// Lines collected between %begin..%end of commands sent via `send_query`.
    /// One Vec<String> per query, in send order.
    pub queries: Receiver<Vec<String>>,
    pending_queries: Arc<AtomicU32>,
}

pub struct StartOptions<'a> {
    pub cwd: Option<&'a str>,
    pub auto_run: Option<&'a str>,
    /// Override the auto-derived session name. Used by tmuxify to keep
    /// one tmux session per desktop.
    pub session_name: Option<&'a str>,
    /// `tmux -L <socket_name>` — isolates the tmux server from any other
    /// tmux instances the user is running. None falls through to tmux's
    /// default socket. The iced spike uses "iced-poc" so its server
    /// can't collide with a native build or the user's day-to-day tmux.
    pub socket_name: Option<&'a str>,
    /// Flusher tick — defaults to 16 ms (~60 Hz).
    pub flush_interval: Duration,
    /// Initial window size. Apps inherit COLUMNS/LINES from this — picking
    /// the visible cell grid here is the only reliable way to make claude /
    /// vim / less wrap to the right width.
    pub cols: u16,
    pub rows: u16,
}

impl Default for StartOptions<'_> {
    fn default() -> Self {
        Self {
            cwd: None,
            auto_run: None,
            session_name: None,
            socket_name: None,
            flush_interval: Duration::from_millis(16),
            cols: 89,
            rows: 28,
        }
    }
}

impl TmuxSession {
    pub fn start(opts: StartOptions<'_>) -> Result<Self> {
        let session_name = opts
            .session_name
            .map(|s| s.to_string())
            .or_else(|| {
                opts.cwd
                    .filter(|p| !p.is_empty())
                    .map(session_name_for_path)
            })
            .unwrap_or_else(|| "tmuxify-main".into());

        // has-session must use the same socket as the spawn below or
        // tmux will lie about session existence.
        let mut has_cmd = Command::new("tmux");
        if let Some(sock) = opts.socket_name {
            has_cmd.args(["-L", sock]);
        }
        let session_exists = has_cmd
            .args(["has-session", "-t", &session_name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let cols_s = opts.cols.to_string();
        let rows_s = opts.rows.to_string();
        let mut cmd = Command::new("tmux");
        if let Some(sock) = opts.socket_name {
            cmd.args(["-L", sock]);
        }
        // Drop $TMUX so a tmux-inside-tmux situation can't reach back to
        // the outer session — without this the inner control-mode tmux
        // exits with "sessions should be nested with care, unset $TMUX".
        cmd.env_remove("TMUX");
        // True-colour pipe: without these tmux negotiates the screen-* TERM
        // family by default, which clamps to 8/16 colours and visibly fades
        // claude's UI palette. Tell tmux's *client* (us) supports RGB, set
        // a 256-colour TERM for the inner shell, and advertise
        // COLORTERM=truecolor so apps (claude / vim / bat / …) emit 24-bit
        // SGR escapes instead of stepping down.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // -f /dev/null: skip the user's ~/.tmux.conf — heavy configs
        // (TPM plugins, custom status-line, terminal-overrides) can
        // interfere with control-mode %output forwarding and leave the
        // inner pane stuck after the first few bytes.
        cmd.args([
            "-f",
            "/dev/null",
            // -T 256,RGB: tell tmux this control-mode client supports 256
            // colours and 24-bit RGB, so apps inside the panes don't get
            // clamped to a smaller palette.
            "-T",
            "256,RGB",
            "-C",
            "new-session",
            "-A",
            "-s",
            &session_name,
            "-x",
            &cols_s,
            "-y",
            &rows_s,
        ]);
        if let Some(p) = opts.cwd.filter(|s| !s.is_empty()) {
            cmd.arg("-c").arg(p);
        }
        // Spawn an interactive login shell so the user's ~/.zshrc / ~/.bashrc
        // loads — otherwise functions/aliases defined there (e.g. a `claude`
        // shell function) aren't available inside the pane.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        cmd.arg(format!("exec {} -il", shell));
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().context("tmux spawn failed")?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let parsers: ParserMap = Arc::new(Mutex::new(HashMap::new()));
        let pane_size = Arc::new(Mutex::new((opts.rows, opts.cols)));
        let prev_rows: Arc<Mutex<HashMap<String, Vec<Row>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (event_tx, event_rx) = unbounded::<TmuxEvent>();
        let (screen_tx, screen_rx) = unbounded::<ScreenUpdate>();
        let (query_tx, query_rx) = unbounded::<Vec<String>>();
        // Dirty-pane signal: the reader posts a pane id every time it
        // pipes a `%output` payload into vt100. The flusher blocks on
        // this channel instead of polling on a fixed interval, so a
        // shell echo round-trips in microseconds instead of waiting up
        // to one tick. Bounded so a runaway pane can't accumulate signals
        // without backpressure; full-channel try_send silently drops
        // because there's already a pending flush for that pane anyway.
        let (dirty_tx, dirty_rx) = crossbeam_channel::bounded::<String>(1024);
        let pending_queries = Arc::new(AtomicU32::new(0));

        spawn_reader(
            stdout,
            parsers.clone(),
            pane_size.clone(),
            event_tx,
            query_tx,
            pending_queries.clone(),
            dirty_tx,
        );
        spawn_flusher(parsers.clone(), prev_rows, screen_tx, dirty_rx, opts.flush_interval);

        if !session_exists {
            if let Some(text) = opts.auto_run.filter(|s| !s.is_empty()) {
                thread::sleep(Duration::from_millis(400));
                let escaped = text.replace('\'', "'\\''");
                let _ = writeln!(stdin, "send-keys -l '{}'", escaped);
                let _ = writeln!(stdin, "send-keys Enter");
                let _ = stdin.flush();
            }
        }

        Ok(Self {
            child,
            stdin: Mutex::new(stdin),
            parsers,
            pane_size,
            socket_name: opts.socket_name.map(|s| s.to_string()),
            session_name,
            events: event_rx,
            screens: screen_rx,
            queries: query_rx,
            pending_queries,
        })
    }

    /// Send a tmux command whose stdout response we want collected. The
    /// response (lines between %begin..%end) arrives on `queries`.
    pub fn send_query(&self, cmd: &str) -> Result<()> {
        self.pending_queries.fetch_add(1, Ordering::SeqCst);
        self.send_cmd(cmd)
    }

    pub fn send_cmd(&self, cmd: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "{cmd}")?;
        stdin.flush()?;
        Ok(())
    }

    /// `send-keys -H <hex>` to a pane. UTF-8 input is hex-encoded by the caller.
    pub fn send_keys_hex(&self, pane_id: Option<&str>, hex: &str) -> Result<()> {
        let target = pane_id
            .map(|p| format!("-t '{p}' "))
            .unwrap_or_default();
        self.send_cmd(&format!("send-keys {target}-H {hex}"))
    }

    pub fn resize_client(&self, cols: u16, rows: u16) -> Result<()> {
        self.send_cmd(&format!("refresh-client -C {cols}x{rows}"))?;
        if let Ok(mut sz) = self.pane_size.lock() {
            *sz = (rows, cols);
        }
        if let Ok(mut map) = self.parsers.lock() {
            for parser in map.values_mut() {
                parser.set_size(rows, cols);
            }
        }
        Ok(())
    }

    pub fn detach(&mut self) {
        let _ = self.send_cmd("detach-client");
        let _ = self.child.wait();
    }
}

impl Drop for TmuxSession {
    /// Tear down our tmux client + the server we spawned. Without this,
    /// `tmux -L kasaterm ...` clients linger across app restarts and
    /// the user accumulates dozens of stale processes. We only run
    /// `kill-server` when `socket_name` is set — otherwise we'd be
    /// shooting the user's day-to-day tmux server.
    fn drop(&mut self) {
        // Best effort: detach our control-mode client first so tmux
        // closes its pipes cleanly, then kill the child if it's still
        // alive, then nuke the isolated socket's server so the shell
        // PIDs underneath it don't outlive the GUI.
        let _ = self.send_cmd("detach-client");
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(sock) = self.socket_name.as_deref() {
            let _ = Command::new("tmux")
                .args(["-L", sock, "kill-server"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    parsers: ParserMap,
    pane_size: Arc<Mutex<(u16, u16)>>,
    events: Sender<TmuxEvent>,
    queries: Sender<Vec<String>>,
    pending_queries: Arc<AtomicU32>,
    dirty_tx: Sender<String>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        // While a %begin..%end pair is open and a query is pending, we
        // capture NonProtocolLine entries here instead of forwarding them
        // through the normal events channel.
        let mut capture: Option<Vec<String>> = None;
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        loop {
            buf.clear();
            // Read raw bytes up to '\n'. Don't use BufRead::lines(),
            // which forces UTF-8 decoding and silently kills the reader
            // thread the first time a line contains an invalid byte —
            // claude's box-drawing paints stream split across many %output
            // lines and occasionally chop UTF-8 codepoints, so even valid
            // output appears invalid at the line boundary.
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            // Strip trailing \r?\n. Decode lossy so invalid bytes become
            // U+FFFD; downstream %output decoder works on the raw escape
            // notation anyway, not on the substituted glyphs.
            while matches!(buf.last(), Some(&b'\n') | Some(&b'\r')) {
                buf.pop();
            }
            // Fast path for %output — its payload is the inner app's raw
            // byte stream (claude paints UTF-8 box drawing chars, CJK,
            // Nerd Font glyphs). Going through String::from_utf8_lossy on
            // the whole line would replace any %output line that ends in
            // a partial multi-byte codepoint with U+FFFD, corrupting the
            // vt100 input. Parse %output ourselves and forward raw bytes.
            let event = if buf.starts_with(b"%output ") {
                let rest = &buf[b"%output ".len()..];
                if let Some(sp) = rest.iter().position(|&b| b == b' ') {
                    let pane = String::from_utf8_lossy(&rest[..sp]).into_owned();
                    let data = crate::event::decode_output_bytes(&rest[sp + 1..]);
                    TmuxEvent::Output { pane_id: pane, data }
                } else {
                    parse_line(&String::from_utf8_lossy(&buf))
                }
            } else {
                // All other tmux control-mode lines are ASCII (`%begin`,
                // `%end`, `%layout-change`, etc) so lossy decode is safe.
                parse_line(&String::from_utf8_lossy(&buf))
            };
            match &event {
                TmuxEvent::Output { pane_id, data } => {
                    let (rows, cols) = *pane_size.lock().unwrap();
                    let mut map = parsers.lock().unwrap();
                    let p = map
                        .entry(pane_id.clone())
                        .or_insert_with(|| vt100::Parser::new(rows, cols, 5000));
                    p.process(&data);
                    drop(map);
                    // Wake the flusher *right after* the parser absorbs
                    // the new bytes. try_send is fine — the bounded
                    // channel just drops duplicate notifications, which
                    // is exactly the coalescing behaviour we want when
                    // an app bursts thousands of small %output frames.
                    let _ = dirty_tx.try_send(pane_id.clone());
                }
                TmuxEvent::Begin { .. } => {
                    if pending_queries.load(Ordering::SeqCst) > 0 && capture.is_none() {
                        capture = Some(Vec::new());
                    } else if events.send(event).is_err() {
                        break;
                    }
                }
                TmuxEvent::End { .. } | TmuxEvent::Error { .. } => {
                    if let Some(buf) = capture.take() {
                        pending_queries.fetch_sub(1, Ordering::SeqCst);
                        let _ = queries.send(buf);
                    } else if events.send(event).is_err() {
                        break;
                    }
                }
                TmuxEvent::NonProtocolLine { raw } => {
                    if let Some(buf) = capture.as_mut() {
                        buf.push(raw.clone());
                    } else if events.send(event).is_err() {
                        break;
                    }
                }
                _ => {
                    if events.send(event).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = events.send(TmuxEvent::Exit);
    });
}

fn spawn_flusher(
    parsers: ParserMap,
    prev_rows: Arc<Mutex<HashMap<String, Vec<Row>>>>,
    out: Sender<ScreenUpdate>,
    dirty_rx: Receiver<String>,
    safety_interval: Duration,
) {
    thread::spawn(move || loop {
        // Block until the reader marks a pane dirty (or until the safety
        // interval, in case a notification was dropped). Coalesce a 2ms
        // burst window — claude / vim can emit hundreds of %output frames
        // in a single tick, batching them into one snapshot per pane keeps
        // the consumer's ScreenUpdate rate bounded without sacrificing
        // latency in the steady state.
        let first_dirty = match dirty_rx.recv_timeout(safety_interval) {
            Ok(pid) => Some(pid),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
        };
        let mut dirty_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(pid) = first_dirty {
            dirty_set.insert(pid);
        }
        let coalesce_until = std::time::Instant::now() + Duration::from_millis(2);
        loop {
            let remaining = coalesce_until.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match dirty_rx.recv_timeout(remaining) {
                Ok(pid) => {
                    dirty_set.insert(pid);
                }
                Err(_) => break,
            }
        }
        if dirty_set.is_empty() {
            // Safety-tick path — re-scan every parser in case a signal
            // was dropped (bounded channel + try_send is best-effort).
            let map = parsers.lock().unwrap();
            dirty_set.extend(map.keys().cloned());
        }
        let pane_ids: Vec<String> = dirty_set.into_iter().collect();
        for pid in pane_ids {
            let snap = {
                let map = parsers.lock().unwrap();
                let Some(parser) = map.get(&pid) else { continue };
                snapshot_screen(&pid, parser)
            };

            let dirty = {
                let mut prev_map = prev_rows.lock().unwrap();
                let prev = prev_map.entry(pid.clone()).or_default();
                let mut dirty: Vec<(u16, Row)> = Vec::new();
                if prev.len() != snap.rows_data.len() {
                    for (i, row) in snap.rows_data.iter().enumerate() {
                        dirty.push((i as u16, row.clone()));
                    }
                } else {
                    for (i, row) in snap.rows_data.iter().enumerate() {
                        if &prev[i] != row {
                            dirty.push((i as u16, row.clone()));
                        }
                    }
                }
                *prev = snap.rows_data;
                dirty
            };

            let update = ScreenUpdate {
                pane_id: pid,
                rows: snap.rows,
                cols: snap.cols,
                dirty,
                cursor_row: snap.cursor_row,
                cursor_col: snap.cursor_col,
                cursor_visible: snap.cursor_visible,
                alt_screen: snap.alt_screen,
                mouse_enabled: snap.mouse_enabled,
                mouse_sgr: snap.mouse_sgr,
                title: snap.title,
                eof: false,
            };
            if out.send(update).is_err() {
                return;
            }
        }
    });
}

struct Snapshot {
    rows: u16,
    cols: u16,
    rows_data: Vec<Row>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    alt_screen: bool,
    mouse_enabled: bool,
    mouse_sgr: bool,
    title: Option<String>,
}

fn snapshot_screen(_pane_id: &str, parser: &vt100::Parser) -> Snapshot {
    let s = parser.screen();
    let (h, w) = s.size();
    let mut rows_data: Vec<Row> = Vec::with_capacity(h as usize);
    for r in 0..h {
        let mut row: Row = Vec::with_capacity(w as usize);
        for c in 0..w {
            let cw = match s.cell(r, c) {
                Some(cell) => vt_cell(cell),
                None => Cell::blank(),
            };
            row.push(cw);
        }
        rows_data.push(row);
    }
    let (cr, cc) = s.cursor_position();
    let title = {
        let t = s.title();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    };
    // Mouse mode plumbing: any non-None MouseProtocolMode means the
    // inner app is interested in mouse events. SGR (1006) encoding is
    // the modern format claude / vim / lazygit speak; legacy X10 / UTF
    // encodings are still readable but rare.
    use vt100::MouseProtocolEncoding;
    use vt100::MouseProtocolMode;
    let mouse_enabled = !matches!(s.mouse_protocol_mode(), MouseProtocolMode::None);
    let mouse_sgr = matches!(s.mouse_protocol_encoding(), MouseProtocolEncoding::Sgr);
    Snapshot {
        rows: h,
        cols: w,
        rows_data,
        cursor_row: cr,
        cursor_col: cc,
        cursor_visible: !s.hide_cursor(),
        alt_screen: s.alternate_screen(),
        mouse_enabled,
        mouse_sgr,
        title,
    }
}

fn session_name_for_path(path: &str) -> String {
    let basename = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("root");
    let mut hash: u64 = 1469598103934665603;
    for b in path.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    let short = format!("{:x}", hash & 0xFFFFFF);
    let safe_base: String = basename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("tmuxify-{}-{}", safe_base, short)
}
