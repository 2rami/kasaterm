//! 키 이름 → 칸에 넣을 바이트. 이름으로 받는 까닭은 부르는 쪽(나쵸)이 칸의 모드를 모르기
//! 때문이다 — 방향키는 칸이 커서 키 모드(DECCKM)를 켰는지에 따라 바이트가 다르다.

/// 모르는 이름이면 `None`. 한 글자는 그 글자 그대로다(물음의 보기 번호를 고를 때).
pub fn bytes(name: &str, app_cursor: bool) -> Option<Vec<u8>> {
    let lower = name.trim().to_ascii_lowercase();
    let arrow = |c: u8| if app_cursor { vec![0x1b, b'O', c] } else { vec![0x1b, b'[', c] };
    let fixed: Option<&[u8]> = match lower.as_str() {
        "enter" | "return" => Some(b"\r"),
        "tab" => Some(b"\t"),
        "shift+tab" | "backtab" => Some(b"\x1b[Z"),
        "escape" | "esc" => Some(b"\x1b"),
        "backspace" => Some(b"\x7f"),
        "delete" | "del" => Some(b"\x1b[3~"),
        "space" => Some(b" "),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        _ => None,
    };
    if let Some(b) = fixed {
        return Some(b.to_vec());
    }
    match lower.as_str() {
        "up" => return Some(arrow(b'A')),
        "down" => return Some(arrow(b'B')),
        "right" => return Some(arrow(b'C')),
        "left" => return Some(arrow(b'D')),
        _ => {}
    }
    if let Some(rest) = lower.strip_prefix("ctrl+") {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() {
                return Some(vec![c as u8 & 0x1f]);
            }
        }
        return None;
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_string().into_bytes()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::bytes;

    #[test]
    fn 방향키는_커서_키_모드를_따른다() {
        assert_eq!(bytes("up", false).unwrap(), b"\x1b[A");
        assert_eq!(bytes("up", true).unwrap(), b"\x1bOA");
        assert_eq!(bytes("Left", true).unwrap(), b"\x1bOD");
    }

    #[test]
    fn 이름_있는_키와_컨트롤() {
        assert_eq!(bytes("enter", false).unwrap(), b"\r");
        assert_eq!(bytes("Escape", false).unwrap(), b"\x1b");
        assert_eq!(bytes("ctrl+c", false).unwrap(), vec![3]);
        assert_eq!(bytes("shift+tab", false).unwrap(), b"\x1b[Z");
    }

    #[test]
    fn 한_글자는_그대로_모르는_이름은_없음() {
        assert_eq!(bytes("2", false).unwrap(), b"2");
        assert_eq!(bytes("가", false).unwrap(), "가".as_bytes());
        assert!(bytes("ctrl+", false).is_none());
        assert!(bytes("ctrl+1", false).is_none());
        assert!(bytes("hyper", false).is_none());
    }
}
