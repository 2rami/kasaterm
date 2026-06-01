//! tmux control mode (-CC) line-event parser.
//!
//! `tmux -C` emits `%`-prefixed lines on stdout. This module decodes
//! each line into [`TmuxEvent`].

#[derive(Debug, Clone, PartialEq)]
pub enum TmuxEvent {
    Begin { ts: String, id: String, flags: String },
    End { ts: String, id: String, flags: String },
    Error { ts: String, id: String, flags: String },
    Output { pane_id: String, data: Vec<u8> },
    WindowAdd { window_id: String },
    WindowClose { window_id: String },
    WindowRenamed { window_id: String, name: String },
    SessionChanged { session_id: String, name: String },
    LayoutChange { window_id: String, layout: String },
    PaneModeChanged { pane_id: String },
    /// tmux 3.x emits `%window-pane-changed <window_id> <pane_id>`
    /// whenever the focused pane within a window changes. UIs use this
    /// to keep their "active pane" highlight in sync.
    WindowPaneChanged { window_id: String, pane_id: String },
    ClientDetached,
    Exit,
    Unknown { raw: String },
    NonProtocolLine { raw: String },
}

pub fn parse_line(line: &str) -> TmuxEvent {
    if !line.starts_with('%') {
        return TmuxEvent::NonProtocolLine { raw: line.to_string() };
    }

    let body = &line[1..];
    let (name, rest) = match body.find(' ') {
        Some(i) => (&body[..i], &body[i + 1..]),
        None => (body, ""),
    };

    match name {
        "begin" => parse_begin_end(rest).map(|(ts, id, fl)| TmuxEvent::Begin { ts, id, flags: fl }),
        "end" => parse_begin_end(rest).map(|(ts, id, fl)| TmuxEvent::End { ts, id, flags: fl }),
        "error" => parse_begin_end(rest).map(|(ts, id, fl)| TmuxEvent::Error { ts, id, flags: fl }),
        "output" => parse_output(rest),
        "window-add" => Some(TmuxEvent::WindowAdd { window_id: rest.to_string() }),
        // tmux 3.4 sometimes sends only the unlinked variant for windows
        // the control-mode client just created. Treat it the same as
        // window-add so we register a tab for it either way.
        "unlinked-window-add" => Some(TmuxEvent::WindowAdd { window_id: rest.to_string() }),
        "window-close" => Some(TmuxEvent::WindowClose { window_id: rest.to_string() }),
        "window-renamed" => split_two(rest)
            .map(|(id, name)| TmuxEvent::WindowRenamed { window_id: id, name }),
        "session-changed" => split_two(rest)
            .map(|(id, name)| TmuxEvent::SessionChanged { session_id: id, name }),
        "layout-change" => split_two(rest)
            .map(|(id, layout)| TmuxEvent::LayoutChange { window_id: id, layout }),
        "pane-mode-changed" => Some(TmuxEvent::PaneModeChanged { pane_id: rest.to_string() }),
        "window-pane-changed" => split_two(rest)
            .map(|(window_id, pane_id)| TmuxEvent::WindowPaneChanged { window_id, pane_id }),
        "client-detached" => Some(TmuxEvent::ClientDetached),
        "exit" => Some(TmuxEvent::Exit),
        _ => None,
    }
    .unwrap_or_else(|| TmuxEvent::Unknown { raw: line.to_string() })
}

fn parse_begin_end(rest: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = rest.splitn(3, ' ').collect();
    if parts.len() < 3 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string(), parts[2].to_string()))
}

fn parse_output(rest: &str) -> Option<TmuxEvent> {
    let (pane, data) = match rest.find(' ') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => return None,
    };
    Some(TmuxEvent::Output {
        pane_id: pane.to_string(),
        data: decode_output(data),
    })
}

// tmux escapes non-printables as `\ooo` (3-digit octal) and `\` as `\\`.
// Return raw bytes — the vt100 parser handles UTF-8 reassembly across
// %output chunks. Decoding to String here would lossy-replace partial
// multi-byte codepoints with U+FFFD and corrupt CJK / box-drawing output
// whenever tmux splits a 3-byte char across two %output events.
fn decode_output(s: &str) -> Vec<u8> {
    decode_output_bytes(s.as_bytes())
}

pub(crate) fn decode_output_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            if n == b'\\' {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if i + 3 < bytes.len()
                && (b'0'..=b'7').contains(&bytes[i + 1])
                && (b'0'..=b'7').contains(&bytes[i + 2])
                && (b'0'..=b'7').contains(&bytes[i + 3])
            {
                let v = ((bytes[i + 1] - b'0') as u16) * 64
                    + ((bytes[i + 2] - b'0') as u16) * 8
                    + ((bytes[i + 3] - b'0') as u16);
                out.push(v as u8);
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

fn split_two(s: &str) -> Option<(String, String)> {
    let i = s.find(' ')?;
    Some((s[..i].to_string(), s[i + 1..].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_begin() {
        match parse_line("%begin 1746940000 12 0") {
            TmuxEvent::Begin { ts, id, flags } => {
                assert_eq!(ts, "1746940000");
                assert_eq!(id, "12");
                assert_eq!(flags, "0");
            }
            e => panic!("expected Begin, got {:?}", e),
        }
    }

    #[test]
    fn parses_output() {
        match parse_line("%output %3 hello world") {
            TmuxEvent::Output { pane_id, data } => {
                assert_eq!(pane_id, "%3");
                assert_eq!(data, b"hello world");
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn decodes_octal_escape() {
        match parse_line("%output %1 \\033[31mhi\\033[0m") {
            TmuxEvent::Output { data, .. } => assert_eq!(data, b"\x1b[31mhi\x1b[0m"),
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn decodes_backslash_escape() {
        match parse_line("%output %1 path\\\\with\\\\backslash") {
            TmuxEvent::Output { data, .. } => assert_eq!(data, b"path\\with\\backslash"),
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn parses_window_add() {
        match parse_line("%window-add @5") {
            TmuxEvent::WindowAdd { window_id } => assert_eq!(window_id, "@5"),
            _ => panic!("expected WindowAdd"),
        }
    }

    #[test]
    fn parses_layout_change() {
        match parse_line("%layout-change @1 c1d8,80x24,0,0,1") {
            TmuxEvent::LayoutChange { window_id, layout } => {
                assert_eq!(window_id, "@1");
                assert_eq!(layout, "c1d8,80x24,0,0,1");
            }
            _ => panic!("expected LayoutChange"),
        }
    }

    #[test]
    fn parses_exit() {
        match parse_line("%exit") {
            TmuxEvent::Exit => {}
            e => panic!("expected Exit, got {:?}", e),
        }
    }

    #[test]
    fn unknown_event_preserved() {
        match parse_line("%future-event foo bar") {
            TmuxEvent::Unknown { raw } => assert_eq!(raw, "%future-event foo bar"),
            e => panic!("expected Unknown, got {:?}", e),
        }
    }

    #[test]
    fn non_protocol_line() {
        match parse_line("normal text") {
            TmuxEvent::NonProtocolLine { raw } => assert_eq!(raw, "normal text"),
            _ => panic!("expected NonProtocolLine"),
        }
    }
}
