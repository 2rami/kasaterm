//! tmux control mode (-CC) 이벤트 파서.
//!
//! tmux 가 `tmux -C attach` 로 띄워지면 stdout 으로 `%`-prefixed 이벤트를
//! 라인 단위로 흘려보낸다. 이 모듈은 그 라인을 enum 으로 디코딩한다.

use serde::Serialize;

/// tmux 가 클라이언트로 흘려보내는 이벤트.
///
/// raw_line 은 디버깅용으로 원본 라인을 그대로 보관.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TmuxEvent {
    /// `%begin <ts> <id> <flags>` — 명령 응답 시작
    Begin { ts: String, id: String, flags: String },
    /// `%end <ts> <id> <flags>` — 명령 응답 끝
    End { ts: String, id: String, flags: String },
    /// `%error <ts> <id> <flags>`
    Error { ts: String, id: String, flags: String },
    /// `%output %<pane-id> <data>` — pane 출력
    Output { pane_id: String, data: String },
    /// `%window-add @<id>`
    WindowAdd { window_id: String },
    /// `%window-close @<id>`
    WindowClose { window_id: String },
    /// `%window-renamed @<id> <name>`
    WindowRenamed { window_id: String, name: String },
    /// `%session-changed $<id> <name>`
    SessionChanged { session_id: String, name: String },
    /// `%layout-change @<id> <layout>`
    LayoutChange { window_id: String, layout: String },
    /// `%pane-mode-changed %<id>`
    PaneModeChanged { pane_id: String },
    /// `%client-detached`
    ClientDetached,
    /// `%exit`
    Exit,
    /// 알 수 없는 % 이벤트 (forward-compat)
    Unknown { raw: String },
    /// % 접두 없는 라인 (정상 동작에선 안 나옴, 보존)
    NonProtocolLine { raw: String },
}

/// 한 라인을 파싱. 라인은 trailing newline 제거된 상태로 들어와야 함.
pub fn parse_line(line: &str) -> TmuxEvent {
    if !line.starts_with('%') {
        return TmuxEvent::NonProtocolLine { raw: line.to_string() };
    }

    // % 다음 첫 토큰이 이벤트 이름
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
        "window-close" => Some(TmuxEvent::WindowClose { window_id: rest.to_string() }),
        "window-renamed" => split_two(rest).map(|(id, name)| TmuxEvent::WindowRenamed {
            window_id: id,
            name,
        }),
        "session-changed" => split_two(rest).map(|(id, name)| TmuxEvent::SessionChanged {
            session_id: id,
            name,
        }),
        "layout-change" => split_two(rest).map(|(id, layout)| TmuxEvent::LayoutChange {
            window_id: id,
            layout,
        }),
        "pane-mode-changed" => Some(TmuxEvent::PaneModeChanged { pane_id: rest.to_string() }),
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
    // `%<pane-id> <data>`
    let (pane, data) = match rest.find(' ') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => return None,
    };
    Some(TmuxEvent::Output {
        pane_id: pane.to_string(),
        data: data.to_string(),
    })
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
        let e = parse_line("%begin 1746940000 12 0");
        match e {
            TmuxEvent::Begin { ts, id, flags } => {
                assert_eq!(ts, "1746940000");
                assert_eq!(id, "12");
                assert_eq!(flags, "0");
            }
            _ => panic!("expected Begin, got {:?}", e),
        }
    }

    #[test]
    fn parses_output() {
        let e = parse_line("%output %3 hello world");
        match e {
            TmuxEvent::Output { pane_id, data } => {
                assert_eq!(pane_id, "%3");
                assert_eq!(data, "hello world");
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn parses_window_add() {
        let e = parse_line("%window-add @5");
        match e {
            TmuxEvent::WindowAdd { window_id } => assert_eq!(window_id, "@5"),
            _ => panic!("expected WindowAdd"),
        }
    }

    #[test]
    fn parses_layout_change() {
        let e = parse_line("%layout-change @1 c1d8,80x24,0,0,1");
        match e {
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
