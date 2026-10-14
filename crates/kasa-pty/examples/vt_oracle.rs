//! Reference interpreter for kasaterm.js differential tests (`web/kasaterm-js`).
//!
//! Feeds recorded PTY bytes into alacritty_terminal the same way the pane reader does
//! (UTF-8 reassembly across reads, NFC per non-ASCII batch, scrollback cap from the column
//! budget) and reports the grid after every chunk so the JS parser can be held to it cell by
//! cell. The JS side computes the same row hashes; see `web/kasaterm-js/test/oracle.mjs`.
//!
//! stdin frames:  `o` u32 len bytes  ·  `r` u16 cols u16 rows  ·  `d` (full dump)  ·  `e` (end)
//! stdout: one JSON line per frame.
//!
//! `vt_oracle widths` prints the cell width alacritty assigns to every code point instead.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, CursorShape, Handler, Processor, StdSyncHandler};
use unicode_normalization::UnicodeNormalization;

// kasa-pty/src/state/vt.rs: history_lines_for_cols with the default 1 GiB budget.
fn history_lines_for_cols(cols: u16) -> usize {
    let per_line = (cols.max(1) as usize) * 24;
    ((1024 * 1024 * 1024) / per_line).clamp(1_000, 100_000)
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<String>>>);

impl EventListener for Events {
    fn send_event(&self, event: Event) {
        let line = match event {
            // kasa-pty answers DA1 as VT220+colour instead of alacritty's VT102.
            Event::PtyWrite(s) if s == "\x1b[?6c" => "w:\x1b[?62;22c".to_string(),
            Event::PtyWrite(s) => format!("w:{s}"),
            Event::ColorRequest(index, _) => format!("color:{index}"),
            Event::TextAreaSizeRequest(_) => "pxsize".to_string(),
            Event::ClipboardStore(kind, text) => format!("clip:{kind:?}:{text}"),
            Event::ClipboardLoad(kind, _) => format!("clipload:{kind:?}"),
            Event::Title(t) => format!("title:{t}"),
            Event::ResetTitle => "title-reset".to_string(),
            Event::Bell => "bell".to_string(),
            _ => return,
        };
        self.0.lock().unwrap().push(line);
    }
}

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn color(c: Color) -> String {
    match c {
        Color::Named(n) => format!("n{}", n as usize),
        Color::Indexed(i) => format!("i{i}"),
        Color::Spec(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
    }
}

fn cell_key(cell: &Cell, out: &mut String) {
    let mut text = String::new();
    text.push(cell.c);
    if let Some(zw) = cell.zerowidth() {
        text.extend(zw.iter());
    }
    out.extend(text.nfc());
    out.push('\u{1}');
    out.push_str(&cell.flags.bits().to_string());
    out.push('\u{1}');
    out.push_str(&color(cell.fg));
    out.push('\u{1}');
    out.push_str(&color(cell.bg));
    out.push('\u{1}');
    match cell.underline_color() {
        Some(c) => out.push_str(&color(c)),
        None => out.push('-'),
    }
    out.push('\u{1}');
    match cell.hyperlink() {
        Some(link) => {
            out.push_str(link.uri());
            if !link.id().ends_with("_alacritty") {
                out.push('\u{3}');
                out.push_str(link.id());
            }
        },
        None => out.push('-'),
    }
    out.push('\u{2}');
}

fn row_key<T>(term: &Term<T>, line: i32) -> String {
    let mut s = String::new();
    let row = &term.grid()[Line(line)];
    for col in 0..term.columns() {
        cell_key(&row[Column(col)], &mut s);
    }
    s
}

fn fnv(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for c in s.chars() {
        h ^= c as u32;
        h = h.wrapping_mul(16777619);
    }
    h
}

fn shape(s: CursorShape) -> &'static str {
    match s {
        CursorShape::Block => "block",
        CursorShape::Underline => "underline",
        CursorShape::Beam => "beam",
        CursorShape::HollowBlock => "hollow",
        CursorShape::Hidden => "hidden",
    }
}

fn state_line<T>(term: &Term<T>, frame: usize, sync: bool, events: &Events) -> String {
    let grid = term.grid();
    let cur = grid.cursor.point;
    let mut out = String::new();
    out.push_str(&format!(
        "{{\"i\":{frame},\"sync\":{sync},\"cols\":{},\"lines\":{},\"cur\":[{},{},{}],\"mode\":{},\"style\":[\"{}\",{}],\"hist\":{}",
        term.columns(),
        term.screen_lines(),
        cur.line.0,
        cur.column.0,
        grid.cursor.input_needs_wrap,
        term.mode().bits(),
        shape(term.cursor_style().shape),
        term.cursor_style().blinking,
        grid.history_size(),
    ));
    out.push_str(",\"rows\":[");
    for line in 0..term.screen_lines() as i32 {
        if line > 0 {
            out.push(',');
        }
        out.push_str(&fnv(&row_key(term, line)).to_string());
    }
    out.push_str("],\"recent\":[");
    let hist = grid.history_size() as i32;
    for (k, line) in (-hist.min(3)..0).enumerate() {
        if k > 0 {
            out.push(',');
        }
        out.push_str(&fnv(&row_key(term, line)).to_string());
    }
    out.push_str("],\"events\":[");
    let mut ev = events.0.lock().unwrap();
    for (k, e) in ev.drain(..).enumerate() {
        if k > 0 {
            out.push(',');
        }
        json_str(&mut out, &e);
    }
    out.push_str("]}");
    out
}

fn dump_line<T>(term: &Term<T>, all_history: bool) -> String {
    let hist = term.grid().history_size() as i32;
    let top = if all_history { -hist } else { 0 };
    let mut out = String::from("{\"dump\":[");
    for (k, line) in (top..term.screen_lines() as i32).enumerate() {
        if k > 0 {
            out.push(',');
        }
        json_str(&mut out, &row_key(term, line));
    }
    out.push_str(&format!("],\"top\":{top}}}"));
    out
}

// Cut a trailing incomplete UTF-8 sequence off the batch and hold it for the next read,
// like kasa-pty's `Utf8Buffer` (broken bytes pass through untouched).
fn take_complete(leftover: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
    leftover.extend_from_slice(data);
    let n = leftover.len();
    let mut cut = n;
    for back in 1..=3.min(n) {
        let i = n - back;
        let b = leftover[i];
        if b & 0xc0 == 0x80 {
            continue;
        }
        let width = if b & 0xe0 == 0xc0 {
            2
        } else if b & 0xf0 == 0xe0 {
            3
        } else if b & 0xf8 == 0xf0 {
            4
        } else {
            1
        };
        if width > 1 && i + width > n {
            cut = i;
        }
        break;
    }
    let out = leftover[..cut].to_vec();
    leftover.drain(..cut);
    out
}

fn widths() {
    let size = alacritty_terminal::term::test::TermSize::new(8, 2);
    let mut term = Term::new(Config::default(), &size, Events::default());
    let mut runs: Vec<(u32, u32, i32)> = Vec::new();
    for cp in 0..=0x10ffffu32 {
        let Some(c) = char::from_u32(cp) else { continue };
        term.goto(0, 2);
        term.clear_line(alacritty_terminal::vte::ansi::LineClearMode::All);
        term.input(c);
        let grid = term.grid();
        let col = grid.cursor.point.column.0;
        let w: i32 = if grid[Line(0)][Column(2)].flags.contains(Flags::WIDE_CHAR) {
            2
        } else if col == 3 {
            1
        } else if grid[Line(0)][Column(1)].zerowidth().is_some_and(|z| z.contains(&c)) {
            0
        } else {
            -1
        };
        match runs.last_mut() {
            Some(last) if last.2 == w && last.1 + 1 == cp => last.1 = cp,
            _ => runs.push((cp, cp, w)),
        }
    }
    let body: Vec<String> =
        runs.iter().filter(|r| r.2 != 1).map(|r| format!("[{},{},{}]", r.0, r.1, r.2)).collect();
    println!("[{}]", body.join(","));
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("widths") {
        widths();
        return;
    }
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    let events = Events::default();
    let mut term: Option<Term<Events>> = None;
    let mut processor: Processor<StdSyncHandler> = Processor::new();
    let mut leftover = Vec::new();
    let mut pos = 0;
    let mut frame = 0;
    while pos < input.len() {
        let tag = input[pos];
        pos += 1;
        match tag {
            b'r' => {
                let cols = u16::from_le_bytes([input[pos], input[pos + 1]]);
                let rows = u16::from_le_bytes([input[pos + 2], input[pos + 3]]);
                pos += 4;
                let size = alacritty_terminal::term::test::TermSize::new(cols as usize, rows as usize);
                match term.as_mut() {
                    None => {
                        let config = Config {
                            scrolling_history: history_lines_for_cols(cols),
                            // The app leaves the kitty keyboard protocol off; kasaterm.js's
                            // `profile: 'xterm'` turns it on and is checked against this.
                            kitty_keyboard: std::env::var_os("KTJS_KITTY").is_some(),
                            ..Config::default()
                        };
                        term = Some(Term::new(config, &size, events.clone()));
                    },
                    Some(t) => {
                        let old_cols = t.columns() as u16;
                        t.resize(size);
                        if cols != old_cols {
                            t.grid_mut().update_history(history_lines_for_cols(cols));
                        }
                    },
                }
            },
            b'o' => {
                let len = u32::from_le_bytes(input[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                let data = &input[pos..pos + len];
                pos += len;
                let batch = take_complete(&mut leftover, data);
                let normalized: Option<String> = if batch.is_ascii() {
                    None
                } else {
                    std::str::from_utf8(&batch).ok().map(|s| s.nfc().collect())
                };
                let fed = normalized.as_deref().map(str::as_bytes).unwrap_or(&batch);
                processor.advance(term.as_mut().expect("resize frame first"), fed);
            },
            b'd' => {
                writeln!(out, "{}", dump_line(term.as_ref().unwrap(), false)).unwrap();
                continue;
            },
            b'e' => {
                let t = term.as_mut().unwrap();
                processor.stop_sync(t);
                writeln!(out, "{}", state_line(t, frame, false, &events)).unwrap();
                writeln!(out, "{}", dump_line(t, true)).unwrap();
                continue;
            },
            other => panic!("unknown frame tag {other}"),
        }
        let t = term.as_ref().unwrap();
        let pending = processor.sync_timeout().sync_timeout().is_some();
        writeln!(out, "{}", state_line(t, frame, pending, &events)).unwrap();
        frame += 1;
    }
}
