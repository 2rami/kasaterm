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

pub(crate) fn port(base: &str) -> Result<u16, ()> {
    // The descriptor is data, never an arbitrary URL to open or connect to.
    base.strip_prefix("http://127.0.0.1:").ok_or(())?
        .trim_end_matches('/').parse::<u16>().ok().filter(|p| *p != 0).ok_or(())
}

pub(crate) fn get(port: u16, route: &str) -> Result<Value, ()> {
    request(port, "GET", route, None)
}

pub(crate) fn request(port: u16, method: &str, route: &str, body: Option<&Value>) -> Result<Value, ()> {
    request_within(port, method, route, body, Duration::from_secs(3))
}

/// 답을 이만큼까지 기다린다. 장부 조회는 바로 오지만 서버가 창을 옮기는 것까지 하고
/// 답하는 길(`/api/ask`)은 한참 걸려, 같은 3초를 물리면 성공한 일을 실패로 읽는다.
pub(crate) fn request_within(port: u16, method: &str, route: &str, body: Option<&Value>, read: Duration) -> Result<Value, ()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|_| ())?;
    stream.set_read_timeout(Some(read)).map_err(|_| ())?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|_| ())?;
    let body = body.map(Value::to_string).unwrap_or_default();
    write!(stream, "{method} {route} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nX-Journal-Request: 1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).map_err(|_| ())?;
    let mut bytes = Vec::new();
    stream.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() > 1024 * 1024 { return Err(()); }
    let split = bytes.windows(4).position(|w| w == b"\r\n\r\n").ok_or(())?;
    let headers = std::str::from_utf8(&bytes[..split]).map_err(|_| ())?;
    if !matches!(headers.lines().next().and_then(|s| s.split_whitespace().nth(1)), Some("200" | "202")) {
        return Err(());
    }
    serde_json::from_slice(&bytes[split + 4..]).map_err(|_| ())
}

pub(crate) fn service(path: &std::path::Path) -> Result<u16, ()> {
    service_named(path, &["request-journal"])
}

/// 묻고 답하는 길(`/api/ask`)만 쓰는 창구. 장부 채팅과 달리 **나쵸 본체**가 직접 받아도 된다.
///
/// 맥미니엔 장부(request-journal)가 없다 — launchd 가 `~/Desktop` 에 못 들어가(TCC) 모듈을
/// 못 찾고 죽는다. 그래서 그 기계의 펫은 대리인 없이 나쵸를 직접 부르고, 나쵸는 자기 이름
/// (`nacho-ask`)으로 답한다. 판·화면 모으기와 조작 실행도 그쪽이 스스로 한다.
///
/// ⚠️ `service()` 를 통째로 열지 않는 이유 — 장부 채팅(`/api/chat`)·요약·열기는 나쵸에 그
/// 길이 없다. 이름을 넓히면 그 기능들이 있는 척하다 조용히 실패한다.
pub(crate) fn ask_service(path: &std::path::Path) -> Result<u16, ()> {
    service_named(path, &["request-journal", "nacho-ask"])
}

fn service_named(path: &std::path::Path, accepted: &[&str]) -> Result<u16, ()> {
    let bytes = std::fs::read(path).map_err(|_| ())?;
    if bytes.len() > 8192 { return Err(()); }
    let descriptor: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if descriptor["version"] != 1 { return Err(()); }
    let base = descriptor["base_url"].as_str().ok_or(())?;
    let port = port(base)?;
    let health = get(port, "/health")?;
    if health["ok"] != true || health["version"] != 1 {
        return Err(());
    }
    let name = health["service"].as_str().ok_or(())?;
    if !accepted.contains(&name) {
        return Err(());
    }
    Ok(port)
}

fn load(action: Action, path: &std::path::Path) -> Result<String, ()> {
    let port = service(path)?;
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

    /// 그 이름을 대는 서버를 세우고, 서술자를 가리켜 두 창구가 각각 무엇을 받는지 본다.
    fn serve_health(service: &'static str) -> (std::path::PathBuf, std::thread::JoinHandle<()>) {
        use std::net::TcpListener;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let thread = std::thread::spawn(move || {
            // 두 창구가 각각 한 번씩 물어 온다.
            for _ in 0..2 {
                let Ok((mut socket, _)) = listener.accept() else { return };
                read_request(&mut socket);
                let body = format!("{{\"ok\":true,\"version\":1,\"service\":\"{service}\"}}");
                let _ = socket.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes(),
                );
            }
        });
        let path = std::env::temp_dir().join(format!("pet-journal-{service}-{port}.json"));
        std::fs::write(&path, format!("{{\"version\":1,\"base_url\":\"http://127.0.0.1:{port}\"}}")).unwrap();
        (path, thread)
    }

    #[test]
    fn only_the_ask_door_accepts_nacho_itself() {
        // 미니엔 장부가 없어 나쵸가 직접 받는다 — 묻는 길만 그 이름을 받아들인다.
        let (path, thread) = serve_health("nacho-ask");
        assert!(ask_service(&path).is_ok(), "묻는 길은 나쵸 본체를 받는다");
        assert!(service(&path).is_err(), "장부 기능은 나쵸에 없다 — 있는 척하면 안 된다");
        std::fs::remove_file(&path).unwrap();
        thread.join().unwrap();

        // 장부가 있는 기계에서는 둘 다 그대로 열린다.
        let (path, thread) = serve_health("request-journal");
        assert!(ask_service(&path).is_ok());
        assert!(service(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
        thread.join().unwrap();
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
