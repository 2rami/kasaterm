use serde_json::Value;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Summary,
    Waiting,
    Open,
}

pub fn intent(text: &str) -> Option<Action> {
    let text: String = text.chars().filter(|c| !c.is_whitespace() && !"?!？。.!".contains(*c)).collect();
    match text.as_str() {
        "내가뭐시켰지" | "내가뭘시켰지" | "시킨일요약" | "요청요약" => Some(Action::Summary),
        "재시작하면뭐달라져" | "재시작하면뭐가달라져" | "반영기다리는일" => Some(Action::Waiting),
        "요청장부열기" | "요청장부열어줘" => Some(Action::Open),
        _ => None,
    }
}

#[derive(Default)]
pub struct Client {
    pending: Option<Receiver<String>>,
}

impl Client {
    pub fn request(&mut self, action: Action, path: PathBuf) -> bool {
        if self.pending.is_some() { return false; }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let text = load(action, &path).unwrap_or_else(|_| {
                "요청 장부에 연결하지 못했어요. 장부 실행 상태를 확인해 주세요.".into()
            });
            let _ = tx.send(text);
        });
        true
    }

    pub fn poll(&mut self) -> Option<String> {
        match self.pending.as_ref()?.try_recv() {
            Ok(text) => { self.pending = None; Some(text) }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                Some("요청 장부를 읽지 못했어요. 다시 눌러 주세요.".into())
            }
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
}

fn port(base: &str) -> Result<u16, ()> {
    // The descriptor is data, never an arbitrary URL to open or connect to.
    base.strip_prefix("http://127.0.0.1:").ok_or(())?
        .trim_end_matches('/').parse::<u16>().ok().filter(|p| *p != 0).ok_or(())
}

fn get(port: u16, route: &str) -> Result<Value, ()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| ())?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).map_err(|_| ())?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|_| ())?;
    write!(stream, "GET {route} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").map_err(|_| ())?;
    let mut bytes = Vec::new();
    stream.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() > 1024 * 1024 { return Err(()); }
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").ok_or(())?;
    let headers = std::str::from_utf8(&bytes[..split]).map_err(|_| ())?;
    if !matches!(headers.lines().next().and_then(|s| s.split_whitespace().nth(1)), Some("200")) {
        return Err(());
    }
    serde_json::from_slice(&bytes[split + 4..]).map_err(|_| ())
}

fn load(action: Action, path: &std::path::Path) -> Result<String, ()> {
    let bytes = std::fs::read(path).map_err(|_| ())?;
    if bytes.len() > 8192 { return Err(()); }
    let descriptor: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if descriptor["version"] != 1 { return Err(()); }
    let base = descriptor["base_url"].as_str().ok_or(())?;
    let port = port(base)?;
    let health = get(port, "/health")?;
    if health["ok"] != true || health["version"] != 1 || health["service"] != "request-journal" {
        return Err(());
    }
    if action == Action::Open {
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("open").arg(format!("http://127.0.0.1:{port}/")).status();
        #[cfg(target_os = "windows")]
        let result = std::process::Command::new("rundll32.exe").arg("url.dll,FileProtocolHandler").arg(format!("http://127.0.0.1:{port}/")).status();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result = std::process::Command::new("xdg-open").arg(format!("http://127.0.0.1:{port}/")).status();
        if !result.map_err(|_| ())?.success() { return Err(()); }
        return Ok("요청 장부를 열었어요.".into());
    }
    let summary = get(port, "/api/summary")?;
    let key = if action == Action::Waiting { "waiting_text" } else { "text" };
    let text = summary[key].as_str().filter(|text| !text.trim().is_empty()).ok_or(())?;
    Ok(text.chars().take(260).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_request(socket: &mut TcpStream) {
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0; 1];
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 1024);
        }
    }

    #[test]
    fn journal_questions_do_not_capture_normal_student_messages() {
        assert_eq!(intent("내가 뭐 시켰지?"), Some(Action::Summary));
        assert_eq!(intent("재시작하면 뭐가 달라져?"), Some(Action::Waiting));
        assert_eq!(intent("재시작하면 뭐가 달라져 기능 만들어줘"), None);
        assert_eq!(intent("여기 버그 고쳐줘"), None);
    }

    #[test]
    fn descriptor_cannot_open_external_or_script_urls() {
        assert_eq!(port("http://127.0.0.1:18769/"), Ok(18769));
        for bad in ["https://example.com", "http://127.0.0.1:1@evil.test", "file:///etc/passwd", "http://127.0.0.1:0", "http://127.0.0.1:80/path"] {
            assert!(port(bad).is_err());
        }
    }

    #[test]
    fn http_reads_bounded_local_json() {
        use std::net::TcpListener;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            read_request(&mut socket);
            socket.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 11\r\n\r\n{\"ok\":true}").unwrap();
        });
        assert_eq!(get(port, "/health").unwrap()["ok"], true);
        thread.join().unwrap();
    }

    #[test]
    fn summary_is_delivered_by_poll_without_duplicate_workers() {
        use std::net::TcpListener;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            for body in [r#"{"ok":true,"service":"request-journal","version":1}"#, r#"{"text":"요청 원문을 기록했어요.","waiting_text":"반영 여부는 확인이 필요해요."}"#] {
                let (mut socket, _) = listener.accept().unwrap();
                read_request(&mut socket);
                write!(socket, "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
            }
        });
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("kasapet-journal-{}-{unique}.json", std::process::id()));
        std::fs::write(&path, format!(r#"{{"version":1,"base_url":"http://127.0.0.1:{port}"}}"#)).unwrap();
        let mut client = Client::default();
        assert!(client.request(Action::Waiting, path.clone()));
        assert!(!client.request(Action::Summary, path.clone()));
        let start = std::time::Instant::now();
        let response = loop {
            if let Some(text) = client.poll() { break text; }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(response, "반영 여부는 확인이 필요해요.");
        assert!(client.pending.is_none());
        thread.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
