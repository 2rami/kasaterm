//! iTerm2 OSC 1337 inline-image scanning + decoding.
//!
//! The VT parser (alacritty/vte) drops OSC 1337 as unhandled, so we
//! intercept the sequence in the PTY reader *before* it reaches the
//! parser — same idea as the OSC 133 prompt sniff, but stateful: an
//! image's base64 payload is tens to hundreds of KB and routinely spans
//! several 8 KiB reads, so a single-pass `find_subslice` can't work.
//!
//! `ImageScanner::feed` is a small state machine. It copies every
//! non-image byte into the caller's passthrough buffer (which then flows
//! to the VT parser exactly as before — Hangul / OSC 133 / box-drawing
//! paths are untouched) and diverts the image bytes into its own buffer,
//! returning a `CapturedImage` once a sequence terminates.

use std::sync::Arc;

use base64::Engine;
use tmux_bridge::screen::DecodedImage;

/// What we match to enter an image sequence. iTerm2 writes
/// `ESC ] 1337 ; File = <args> : <base64> (BEL|ST)`.
const START: &[u8] = b"\x1b]1337;File=";

/// Hard cap on a single sequence's accumulated bytes (base64 + args).
/// A malformed stream that never terminates would otherwise grow this
/// buffer without bound. 48 MiB of base64 ≈ a 36 MiB image — far past
/// anything sane for a terminal preview.
const MAX_PAYLOAD: usize = 48 * 1024 * 1024;

/// A decoded image plus the size hints parsed from the OSC args. The
/// reader turns the hints into a concrete cell span using the grid +
/// cell-pixel metrics it owns.
pub struct CapturedImage {
    pub image: Arc<DecodedImage>,
    /// `width`/`height` given as a plain cell count (`width=10`).
    pub want_cols: Option<u16>,
    pub want_rows: Option<u16>,
    /// `width`/`height` given in pixels (`width=200px`).
    pub px_cols: Option<u32>,
    pub px_rows: Option<u32>,
    /// `width`/`height` given as a percent of the terminal (`width=50%`).
    pub pct_cols: Option<u16>,
    pub pct_rows: Option<u16>,
}

enum State {
    /// Scanning for START; `matched` = bytes of START seen so far. The
    /// partially-matched prefix is held back (not yet passed through) so
    /// a START split across reads still matches.
    Idle { matched: usize },
    /// Inside the sequence, accumulating until BEL (0x07) or ST (ESC \).
    /// `esc` = the previous body byte was ESC, so the next `\` closes it.
    Body { buf: Vec<u8>, esc: bool },
}

pub struct ImageScanner {
    state: State,
}

impl ImageScanner {
    pub fn new() -> Self {
        Self { state: State::Idle { matched: 0 } }
    }

    /// Feed one raw PTY read. Appends every non-image byte to `out` in
    /// order, and returns any images whose sequences completed here.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) -> Vec<CapturedImage> {
        let mut images = Vec::new();
        let mut i = 0;
        while i < data.len() {
            match &mut self.state {
                State::Idle { matched } => {
                    let b = data[i];
                    if b == START[*matched] {
                        *matched += 1;
                        i += 1;
                        if *matched == START.len() {
                            self.state = State::Body { buf: Vec::new(), esc: false };
                        }
                    } else if *matched > 0 {
                        // Mismatch mid-prefix: flush what we held back and
                        // re-examine this byte from scratch (don't advance
                        // i) so a fresh START can begin on it.
                        out.extend_from_slice(&START[..*matched]);
                        *matched = 0;
                    } else {
                        out.push(b);
                        i += 1;
                    }
                }
                State::Body { buf, esc } => {
                    let b = data[i];
                    i += 1;
                    if *esc {
                        *esc = false;
                        if b == b'\\' {
                            // ST terminator.
                            if let Some(img) = parse_and_decode(buf) {
                                images.push(img);
                            }
                            self.state = State::Idle { matched: 0 };
                            continue;
                        }
                        // A lone ESC that wasn't ST — keep it, then handle b.
                        buf.push(0x1b);
                    }
                    if b == 0x07 {
                        // BEL terminator.
                        if let Some(img) = parse_and_decode(buf) {
                            images.push(img);
                        }
                        self.state = State::Idle { matched: 0 };
                    } else if b == 0x1b {
                        *esc = true;
                    } else {
                        buf.push(b);
                        if buf.len() > MAX_PAYLOAD {
                            eprintln!(
                                "[pty-backend] OSC 1337 payload exceeded {MAX_PAYLOAD} bytes; aborting capture"
                            );
                            self.state = State::Idle { matched: 0 };
                        }
                    }
                }
            }
        }
        images
    }
}

/// Parse the captured body (`<args>:<base64>`) and decode the image.
/// `body` starts immediately after `File=`. Returns `None` on any parse
/// / decode failure — a bad sequence just doesn't show an image.
fn parse_and_decode(body: &[u8]) -> Option<CapturedImage> {
    // Split at the first ':' — the base64 alphabet never contains one.
    let sep = body.iter().position(|&b| b == b':')?;
    let args = &body[..sep];
    let b64 = &body[sep + 1..];

    let mut cap = CapturedImage {
        image: Arc::new(DecodedImage { rgba: Vec::new(), width: 0, height: 0 }),
        want_cols: None,
        want_rows: None,
        px_cols: None,
        px_rows: None,
        pct_cols: None,
        pct_rows: None,
    };

    if let Ok(args_str) = std::str::from_utf8(args) {
        for kv in args_str.split(';') {
            let mut it = kv.splitn(2, '=');
            let key = it.next().unwrap_or("").trim();
            let val = it.next().unwrap_or("").trim();
            match key {
                "width" => apply_dim(val, &mut cap.want_cols, &mut cap.px_cols, &mut cap.pct_cols),
                "height" => apply_dim(val, &mut cap.want_rows, &mut cap.px_rows, &mut cap.pct_rows),
                _ => {}
            }
        }
    }

    // base64 may be wrapped with newlines/whitespace by some senders.
    let cleaned: Vec<u8> = b64
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .ok()?;
    let dynimg = image::load_from_memory(&bytes).ok()?;
    let rgba = dynimg.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    if w == 0 || h == 0 {
        return None;
    }
    cap.image = Arc::new(DecodedImage { rgba: rgba.into_raw(), width: w, height: h });
    Some(cap)
}

/// Parse one iTerm2 dimension value into the right bucket. Forms:
/// `N` (cells), `Npx` (pixels), `N%` (percent), `auto`/empty (none).
fn apply_dim(val: &str, cells: &mut Option<u16>, px: &mut Option<u32>, pct: &mut Option<u16>) {
    if val.is_empty() || val.eq_ignore_ascii_case("auto") {
        return;
    }
    if let Some(n) = val.strip_suffix("px") {
        if let Ok(v) = n.trim().parse::<u32>() {
            *px = Some(v);
        }
    } else if let Some(n) = val.strip_suffix('%') {
        if let Ok(v) = n.trim().parse::<u16>() {
            *pct = Some(v);
        }
    } else if let Ok(v) = val.parse::<u16>() {
        *cells = Some(v);
    }
}
