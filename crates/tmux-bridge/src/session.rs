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

        let session_exists = Command::new("tmux")
            .args(["has-session", "-t", &session_name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let cols_s = opts.cols.to_string();
        let rows_s = opts.rows.to_string();
        let mut cmd = Command::new("tmux");
        cmd.args([
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
        let pending_queries = Arc::new(AtomicU32::new(0));

        spawn_reader(
            stdout,
            parsers.clone(),
            pane_size.clone(),
            event_tx,
            query_tx,
            pending_queries.clone(),
        );
        spawn_flusher(parsers.clone(), prev_rows, screen_tx, opts.flush_interval);

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

fn spawn_reader(
    stdout: std::process::ChildStdout,
    parsers: ParserMap,
    pane_size: Arc<Mutex<(u16, u16)>>,
    events: Sender<TmuxEvent>,
    queries: Sender<Vec<String>>,
    pending_queries: Arc<AtomicU32>,
) {
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        // While a %begin..%end pair is open and a query is pending, we
        // capture NonProtocolLine entries here instead of forwarding them
        // through the normal events channel.
        let mut capture: Option<Vec<String>> = None;
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let event = parse_line(&line);
            match &event {
                TmuxEvent::Output { pane_id, data } => {
                    let (rows, cols) = *pane_size.lock().unwrap();
                    let mut map = parsers.lock().unwrap();
                    let p = map
                        .entry(pane_id.clone())
                        .or_insert_with(|| vt100::Parser::new(rows, cols, 5000));
                    p.process(data.as_bytes());
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
    interval: Duration,
) {
    thread::spawn(move || loop {
        thread::sleep(interval);
        let pane_ids: Vec<String> = {
            let map = parsers.lock().unwrap();
            map.keys().cloned().collect()
        };
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
                title: snap.title,
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
    Snapshot {
        rows: h,
        cols: w,
        rows_data,
        cursor_row: cr,
        cursor_col: cc,
        cursor_visible: !s.hide_cursor(),
        alt_screen: s.alternate_screen(),
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
