//! 카사브라우저의 로컬 창구에 붙는다 — kb 와 같은 길(kasabrowser `internal/bridge`). 맥은 데이터 폴더의 `agent.sock`,
//! Windows 는 `agent.endpoint` 의 루프백 주소에 토큰을 보내지 않고 HMAC 으로 서로 증명한 뒤 JSON 한 줄씩 주고받는다.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::path::PathBuf;
use std::time::Duration;

use ring::hmac;
use serde_json::{json, Value};

use super::browser::{Browser, BrowserError, Device, Op, Read, Status};

const AGENT: &str = "kasaterm-sync";
const WAIT: Duration = Duration::from_secs(50);
const HANDSHAKE: Duration = Duration::from_secs(10);
/// 응답 한도 — 북마크 수만 개와 탭 목록이 들어가는 크기.
const MAX_RESPONSE: u64 = 64 << 20;

/// 매일 브라우저(MyGo 판)의 데이터 폴더. 시험·격리 실행만 다른 폴더를 줄 수 있다.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(any(test, feature = "test-isolation")) {
        return std::env::var_os("KASATERM_BROWSER_DATA_DIR").map(PathBuf::from);
    }
    #[cfg(target_os = "macos")]
    {
        Some(kasa_socket::home_dir()?.join("Library").join("Application Support").join("Kasa Browser OSR"))
    }
    #[cfg(windows)]
    {
        Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Kasa Browser OSR"))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        None
    }
}

pub struct Bridge {
    dir: PathBuf,
}

impl Bridge {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn call(&self, name: &str, args: Value) -> Result<Value, BrowserError> {
        let request = json!({"id": AGENT, "method": "command", "params": {"name": name, "args": args}, "agent": AGENT});
        let mut line = serde_json::to_vec(&request).map_err(|e| BrowserError::Failed(e.to_string()))?;
        line.push(b'\n');
        let response = exchange(&self.dir, &line)?;
        let response: Value = serde_json::from_slice(&response).map_err(|_| BrowserError::Failed("bad response".into()))?;
        if response["ok"].as_bool() == Some(true) {
            return Ok(response["result"].clone());
        }
        let error = response["error"].as_str().unwrap_or_default();
        if error.starts_with("SYNC_STALE") {
            return Err(BrowserError::Stale);
        }
        Err(BrowserError::Failed(error.chars().take(200).collect()))
    }

    /// 카사브라우저 설정 「동기화」에 보일 상태(`display`). 동기화 엔진과 따로 — 맞추기가 멈춰도 상태는 간다.
    pub fn put_state(&self, state: &Value) -> Result<(), BrowserError> {
        self.call("sync.state.put", json!({"state": state})).map(|_| ())
    }

    fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, BrowserError> {
        serde_json::from_value(value).map_err(|_| BrowserError::Failed("bad response".into()))
    }
}

impl Browser for Bridge {
    fn status(&self) -> Result<Status, BrowserError> {
        Self::parse(self.call("sync.status", json!({}))?)
    }

    fn read(&self) -> Result<Read, BrowserError> {
        Self::parse(self.call("sync.read", json!({}))?)
    }

    fn apply(&self, collection: &str, base_rev: &str, ops: &[Op]) -> Result<String, BrowserError> {
        let result = self.call("sync.apply", json!({"collection": collection, "base_rev": base_rev, "ops": ops}))?;
        Ok(result["rev"].as_str().unwrap_or_default().to_string())
    }

    fn put_tabs(&self, devices: &[Device]) -> Result<(), BrowserError> {
        self.call("sync.tabs.put", json!({"devices": devices})).map(|_| ())
    }
}

fn read_line(reader: &mut impl BufRead) -> Result<Vec<u8>, BrowserError> {
    let mut line = Vec::new();
    reader.take(MAX_RESPONSE).read_until(b'\n', &mut line).map_err(|_| BrowserError::NotRunning)?;
    if line.last() != Some(&b'\n') {
        return Err(BrowserError::Failed("no response".into()));
    }
    Ok(line)
}

#[cfg(unix)]
fn exchange(dir: &std::path::Path, request: &[u8]) -> Result<Vec<u8>, BrowserError> {
    use std::os::unix::fs::MetadataExt as _;
    // 남이 쓸 수 있는 폴더면 소켓을 바꿔치기할 수 있다 — kb 와 같이 붙지 않는다.
    let meta = std::fs::metadata(dir).map_err(|_| BrowserError::NotRunning)?;
    if meta.mode() & 0o022 != 0 {
        return Err(BrowserError::Failed("unsafe browser data folder".into()));
    }
    let mut stream = std::os::unix::net::UnixStream::connect(dir.join("agent.sock")).map_err(|_| BrowserError::NotRunning)?;
    stream.set_read_timeout(Some(WAIT)).ok();
    stream.set_write_timeout(Some(WAIT)).ok();
    stream.write_all(request).map_err(|_| BrowserError::NotRunning)?;
    read_line(&mut BufReader::new(stream))
}

#[cfg(windows)]
fn exchange(dir: &std::path::Path, request: &[u8]) -> Result<Vec<u8>, BrowserError> {
    let endpoint: Value = std::fs::read(dir.join("agent.endpoint"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(BrowserError::NotRunning)?;
    let address: std::net::SocketAddr = endpoint["address"].as_str().and_then(|a| a.parse().ok()).ok_or(BrowserError::NotRunning)?;
    if !address.ip().is_loopback() {
        return Err(BrowserError::Failed("browser endpoint is not a loopback address".into()));
    }
    let token = endpoint["token"].as_str().ok_or(BrowserError::NotRunning)?.as_bytes().to_vec();
    let stream = std::net::TcpStream::connect_timeout(&address, HANDSHAKE).map_err(|_| BrowserError::NotRunning)?;
    stream.set_read_timeout(Some(HANDSHAKE)).ok();
    stream.set_write_timeout(Some(HANDSHAKE)).ok();
    let mut writer = stream.try_clone().map_err(|_| BrowserError::NotRunning)?;
    let mut reader = BufReader::new(stream);
    handshake(&mut reader, &mut writer, &token)?;
    reader.get_ref().set_read_timeout(Some(WAIT)).ok();
    writer.set_write_timeout(Some(WAIT)).ok();
    writer.write_all(request).map_err(|_| BrowserError::NotRunning)?;
    read_line(&mut reader)
}

#[cfg(not(any(unix, windows)))]
fn exchange(_: &std::path::Path, _: &[u8]) -> Result<Vec<u8>, BrowserError> {
    Err(BrowserError::NotRunning)
}

/// 증명이 덮는 글. 역할 이름이 달라 브라우저의 증명을 이쪽 증명으로 되돌려 쓰지 못한다.
fn proof_message(role: &str, first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut message = format!("kasa-browser-bridge-v1\0{role}\0").into_bytes();
    message.extend_from_slice(first);
    message.extend_from_slice(second);
    message
}

fn proof(token: &[u8], role: &str, first: &[u8], second: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, token), &proof_message(role, first, second)).as_ref().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// 브라우저가 먼저 토큰을 안다고 증명하기 전에는 무작위 값 말고는 아무것도 보내지 않는다.
fn handshake(reader: &mut impl BufRead, writer: &mut impl Write, token: &[u8]) -> Result<(), BrowserError> {
    let mut nonce = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).map_err(|_| BrowserError::Failed("random".into()))?;
    writer.write_all(format!("{}\n", hex(&nonce)).as_bytes()).map_err(|_| BrowserError::NotRunning)?;
    let mut line = String::new();
    reader.take(256).read_line(&mut line).map_err(|_| BrowserError::NotRunning)?;
    let parts: Vec<&str> = line.split_whitespace().collect();
    let (Some(server_nonce), Some(server_proof)) = (parts.first().and_then(|p| unhex(p)), parts.get(1).and_then(|p| unhex(p))) else {
        return Err(BrowserError::Failed("bad handshake".into()));
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, token);
    if parts.len() != 2 || server_nonce.len() != 32 || hmac::verify(&key, &proof_message("server", &nonce, &server_nonce), &server_proof).is_err() {
        return Err(BrowserError::Failed("browser endpoint failed authentication; nothing was sent".into()));
    }
    let answer = proof(token, "client", &server_nonce, &nonce);
    writer.write_all(format!("{}\n", hex(&answer)).as_bytes()).map_err(|_| BrowserError::NotRunning)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 진짜 Go 창구에 붙어 본다 — `KASATERM_BROWSER_DATA_DIR` 에 kasabrowser `bridge.Listen` 으로 띄운 가짜 sync 서버가
    /// 있을 때 손으로 돌린다(`cargo test -p kasa-device --lib bridge -- --ignored`). 그 서버는 sync.read 에 모음 하나를,
    /// sync.apply 에는 base_rev 가 `e.1` 일 때만 `e.2` 를 준다.
    #[test]
    #[ignore]
    fn talks_to_the_go_bridge() {
        let bridge = Bridge::new(data_dir().expect("KASATERM_BROWSER_DATA_DIR"));
        let read = bridge.read().unwrap();
        assert_eq!(read.collections[0].marks[0].title, "하나");
        assert_eq!(bridge.apply("kasatest-sync@example.test", "e.1", &[]).unwrap(), "e.2");
        assert_eq!(bridge.apply("kasatest-sync@example.test", "e.0", &[]), Err(BrowserError::Stale));
    }

    /// 브라우저 쪽 인사(`authenticatedConn.handshake`)를 그대로 흉내 내 서로 증명하는지 본다.
    #[test]
    fn the_handshake_proves_both_sides_and_refuses_a_wrong_token() {
        let token = b"kasatest-token".to_vec();
        for (server_token, ok) in [(token.clone(), true), (b"other".to_vec(), false)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let client_side = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (server_side, _) = listener.accept().unwrap();
            let token = token.clone();
            let server = std::thread::spawn(move || {
                let mut reader = BufReader::new(server_side.try_clone().unwrap());
                let mut writer = server_side;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let client_nonce = unhex(line.trim()).unwrap();
                let server_nonce = [7u8; 32];
                writer.write_all(format!("{} {}\n", hex(&server_nonce), hex(&proof(&server_token, "server", &client_nonce, &server_nonce))).as_bytes()).unwrap();
                let mut answer = String::new();
                let _ = reader.read_line(&mut answer);
                unhex(answer.trim()).is_some_and(|a| a == proof(&server_token, "client", &server_nonce, &client_nonce))
            });
            let mut reader = BufReader::new(client_side.try_clone().unwrap());
            let mut writer = client_side;
            let result = handshake(&mut reader, &mut writer, &token);
            drop(writer);
            drop(reader);
            assert_eq!(result.is_ok(), ok);
            assert_eq!(server.join().unwrap(), ok, "틀린 토큰의 브라우저에는 증명을 보내지 않는다");
        }
    }
}
