//! `kasaterm-open` — 칸 안 CLI 가 여는 URL 을 사람이 보는 브라우저로. 칸 셰임 폴더에 kasaterm-cli 를 이 이름으로
//! 두고 Windows 칸의 `BROWSER` 를 가리키게 한다(맥은 `open` 셰임이 같은 일을 한다). Claude Code·gh·Python 은
//! `BROWSER` 가 있으면 그 프로그램에 URL 하나를 넘긴다. 앱의 `/open-url` 이 그 칸을 보는 기기(거울·폰)로 돌리고,
//! 앱에 못 닿으면 원래 브라우저로 연다 — 잃는 것이 없다.

use std::io::{Read, Write};

/// 이 실행이 `kasaterm-open` 이름으로 불렸나(멀티콜).
pub(super) fn invoked_as_opener() -> bool {
    std::env::args_os()
        .next()
        .map(std::path::PathBuf::from)
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_ascii_lowercase()))
        .is_some_and(|stem| stem == "kasaterm-open")
}

pub(super) fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url] = args.as_slice() else {
        eprintln!("usage: kasaterm-open URL");
        return 2;
    };
    let web = url.starts_with("http://") || url.starts_with("https://");
    if web {
        if let Some(reply) = ask_app(url) {
            if let Some(link) = reply.get("url").and_then(|v| v.as_str()).filter(|_| reply["target"] == "phone") {
                println!("폰 쪽지로 보냈어요: {link}");
            }
            return 0;
        }
    }
    match open_natively(url) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kasaterm-open: {e}");
            1
        }
    }
}

/// 앱의 `/open-url` 에 맡긴다. 폰 도착지면 앱이 임시 터널을 세우느라 수십 초 걸릴 수 있다.
fn ask_app(url: &str) -> Option<serde_json::Value> {
    let port: u16 = std::env::var("KASASPACE_MCP_PORT").ok()?.trim().parse().ok()?;
    ask_app_at(port, &std::env::var("KASATERM_PANE_ID").unwrap_or_default(), url)
}

fn ask_app_at(port: u16, pane: &str, url: &str) -> Option<serde_json::Value> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(75))).ok()?;
    let request = format!(
        "GET /open-url?url={}&pane={} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n",
        query_escape(url),
        query_escape(pane)
    );
    stream.write_all(request.as_bytes()).ok()?;
    // 답을 다 보낸 쪽이 바로 끊으면 Windows 는 끝을 RST 로 알린다 — 그 전까지 받은 것은 쓴다.
    let mut raw = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200").then_some(())?;
    // 몸통이 조각(chunked)으로 와도 JSON 은 한 덩어리다 — 첫 `{` 부터 마지막 `}` 까지.
    let json = &body[body.find('{')?..=body.rfind('}')?];
    serde_json::from_str::<serde_json::Value>(json).ok().filter(|v| v["ok"] == true)
}

fn query_escape(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// 사람이 원래 쓰던 길 — 칸을 띄운 앱이 덮기 전의 `BROWSER`, 없으면 OS 기본 열기(Windows 는 Claude Code·gh 가
/// 스스로 쓰는 `url.dll,FileProtocolHandler`).
fn open_natively(url: &str) -> std::io::Result<()> {
    let native = std::env::var("KASATERM_NATIVE_BROWSER").ok().filter(|b| !b.trim().is_empty());
    let mut command = match native {
        Some(browser) => {
            let mut c = std::process::Command::new(browser);
            c.arg(url);
            c
        }
        None if cfg!(windows) => {
            let mut c = std::process::Command::new("rundll32.exe");
            c.args(["url.dll,FileProtocolHandler", url]);
            c
        }
        None => {
            let mut c = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" });
            c.arg(url);
            c
        }
    };
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("opener exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_values_are_fully_escaped() {
        assert_eq!(query_escape("https://a.b/c?d=1&e=한"), "https%3A%2F%2Fa.b%2Fc%3Fd%3D1%26e%3D%ED%95%9C");
        assert_eq!(query_escape("%3"), "%253");
    }

    /// 앱이 받은 요청과 돌려준 답을 그대로 맞춰 본다 — 칸 번호를 싣고, 폰 도착이 아니면 링크를 안 찍는다.
    #[test]
    fn the_app_gets_the_url_and_the_pane() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = conn.read(&mut buf).unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"ok":true,"url":"https://x.test/","target":"mac"}"#;
            write!(conn, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len()).unwrap();
            head
        });
        let reply = ask_app_at(port, "%7", "https://x.test/").expect("앱 답");
        let head = server.join().unwrap();
        assert!(head.starts_with("GET /open-url?url=https%3A%2F%2Fx.test%2F&pane=%257 HTTP/1.1"), "{head}");
        assert_eq!(reply["target"], "mac");
    }
}
