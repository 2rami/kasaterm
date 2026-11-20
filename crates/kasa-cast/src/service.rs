//! 앱 ↔ 화면 서비스 제어 규약(`docs/screen-service-split.md` 「앱 ↔ 서비스 제어 길」) — 한 요청에 한 답(JSON).
//! 틀은 도우미 파이프와 같은 `[종류 u8][길이 u32 LE][본문]`, 종류는 늘 `J`. 길은 OS 마다 다르다(Windows 이름 파이프,
//! macOS 유닉스 소켓).

use std::io::{Read, Write};

use serde_json::Value;

/// 제어 규약 판. 앱은 서비스 `hello` 의 값이 다르면 이 서비스를 안 쓰고 앱 안에서 보낸다.
pub const PROTO: u32 = 1;
/// Windows 제어 파이프 이름(`\\.\pipe\…`). 시험 서비스 리그는 `KASATERM_SCREEN_CTL` 로 가른다.
pub const CTL_PIPE: &str = "kasaterm-screen-ctl";
const MAX_BODY: usize = 1 << 20;

pub fn send(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let body = v.to_string();
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(b'J');
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(body.as_bytes());
    w.write_all(&buf)?;
    w.flush()
}

pub fn recv(r: &mut impl Read) -> std::io::Result<Value> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if head[0] != b'J' || len > MAX_BODY {
        return Err(std::io::Error::other("제어 틀이 아니다"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

/// 요청 하나를 보내고 답 하나를 받는다.
pub fn ask(io: &mut (impl Read + Write), v: &Value) -> std::io::Result<Value> {
    send(io, v)?;
    recv(io)
}

/// 제어 이름 — 영문·숫자·`-` 만(파이프·소켓 이름에 바로 쓴다).
pub fn ctl_name() -> String {
    std::env::var("KASATERM_SCREEN_CTL")
        .ok()
        .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or_else(|| CTL_PIPE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frames_round_trip_and_refuse_other_kinds() {
        let mut buf = Vec::new();
        send(&mut buf, &json!({ "t": "status" })).unwrap();
        assert_eq!(recv(&mut buf.as_slice()).unwrap(), json!({ "t": "status" }));
        buf[0] = b'F';
        assert!(recv(&mut buf.as_slice()).is_err());
    }
}
