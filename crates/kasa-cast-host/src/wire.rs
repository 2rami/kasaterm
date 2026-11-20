//! 화면 도우미 틀 — `[종류 u8][길이 u32 LE][본문]`, 조작은 JSON(`d` 화면 번호 + 화면 거울 조작). Windows 도우미 파이프(`win_agent`)와
//! 맥 도우미 소켓(`local`)이 같이 쓴다.

#![cfg_attr(not(any(windows, unix)), allow(dead_code))]

use std::io::{Read, Write};

use kasa_cast::mirror::{Denied, Input};
use serde_json::{json, Value};

const MAX_BODY: usize = 64 << 20;

pub(crate) fn send(w: &mut impl Write, kind: u8, body: &[u8]) -> std::io::Result<()> {
    let mut msg = Vec::with_capacity(body.len() + 5);
    msg.push(kind);
    msg.extend_from_slice(&(body.len() as u32).to_le_bytes());
    msg.extend_from_slice(body);
    w.write_all(&msg)
}

pub(crate) fn recv(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_BODY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

pub(crate) fn input_json(display: u32, input: &Input) -> Value {
    match input {
        Input::Pointer { x, y, buttons, prev, mods } => {
            json!({ "d": display, "t": "pointer", "x": x, "y": y, "b": buttons, "p": prev, "m": mods })
        }
        Input::Scroll { x, y, dx, dy } => json!({ "d": display, "t": "scroll", "x": x, "y": y, "dx": dx, "dy": dy }),
        Input::Key { code, down, mods } => json!({ "d": display, "t": "key", "code": code, "down": down, "m": mods }),
        Input::Text(text) => json!({ "d": display, "t": "text", "s": text }),
    }
}

pub(crate) fn input_from(v: &Value) -> Option<(u32, Input)> {
    let display = u32::try_from(v.get("d")?.as_u64()?).ok()?;
    let unit = |k: &str| v.get(k).and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0));
    let small = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0).min(255) as u8;
    let wheel = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0).clamp(-10_000, 10_000) as i32;
    let input = match v.get("t")?.as_str()? {
        "pointer" => Input::Pointer { x: unit("x")?, y: unit("y")?, buttons: small("b") & 7, prev: small("p") & 7, mods: small("m") & 15 },
        "scroll" => Input::Scroll { x: unit("x")?, y: unit("y")?, dx: wheel("dx"), dy: wheel("dy") },
        "key" => Input::Key {
            code: v.get("code")?.as_str()?.chars().take(32).collect(),
            down: v.get("down").and_then(Value::as_bool),
            mods: small("m") & 15,
        },
        "text" => Input::Text(v.get("s")?.as_str()?.chars().take(4096).collect()),
        _ => return None,
    };
    Some((display, input))
}

pub(crate) fn denied_of(reason: &[u8]) -> Denied {
    if reason == b"permission" {
        Denied::Permission
    } else {
        Denied::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_round_trip_through_the_pipe_json() {
        let all = [
            Input::Pointer { x: 0.25, y: 1.0, buttons: 1, prev: 0, mods: 9 },
            Input::Scroll { x: 0.5, y: 0.5, dx: -3, dy: 120 },
            Input::Key { code: "KeyA".into(), down: Some(true), mods: 2 },
            Input::Key { code: "Enter".into(), down: None, mods: 0 },
            Input::Text("한글 abc".into()),
        ];
        for input in all {
            assert_eq!(input_from(&input_json(3, &input)), Some((3, input)));
        }
        assert_eq!(input_from(&json!({ "d": 0, "t": "pointer", "x": 9.0, "y": -1.0 })).map(|x| x.1), Some(Input::Pointer { x: 1.0, y: 0.0, buttons: 0, prev: 0, mods: 0 }));
        assert_eq!(input_from(&json!({ "t": "text", "s": "x" })), None, "화면 번호 없는 조작은 버린다");
    }

    #[test]
    fn messages_frame_with_kind_and_length() {
        let mut buf = Vec::new();
        send(&mut buf, b'K', &7u64.to_le_bytes()).unwrap();
        send(&mut buf, b'H', &[]).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(recv(&mut r).unwrap(), (b'K', 7u64.to_le_bytes().to_vec()));
        assert_eq!(recv(&mut r).unwrap(), (b'H', Vec::new()));
        assert!(recv(&mut r).is_err());
    }
}
