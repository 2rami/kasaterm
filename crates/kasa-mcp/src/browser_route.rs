//! Browser-side localhost routing. Only a registered source's loopback is forwarded.
//! No browser is opened here, no SSH command is executed remotely, and no SSH
//! settings/keys are discovered or changed. Ordinary SSH forwarding policy wins.
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::{http::StatusCode, response::IntoResponse, Json};
use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub source_machine: String,
    #[serde(default)]
    pub source_machine_id: Option<String>,
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct ResolveResponse {
    pub ok: bool,
    pub url: String,
    pub forwarded: bool,
    pub local_port: u16,
    pub source_port: u16,
}

#[derive(Debug)]
struct RouteError(StatusCode, &'static str);
type Result<T> = std::result::Result<T, RouteError>;

fn bad(message: &'static str) -> RouteError { RouteError(StatusCode::BAD_REQUEST, message) }
fn unavailable(message: &'static str) -> RouteError { RouteError(StatusCode::BAD_GATEWAY, message) }

fn loopback_host(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    if matches!(host, "localhost" | "localhost.") { return Some("127.0.0.1".into()); }
    match host.trim_matches(['[', ']']).parse::<IpAddr>().ok()? {
        IpAddr::V4(ip) if ip.is_loopback() => Some(ip.to_string()),
        IpAddr::V6(ip) if ip.is_loopback() => Some(format!("[{ip}]")),
        _ => None,
    }
}

/// Detection only: unsupported schemes still count as local and then fail closed
/// in the resolver rather than being opened on the wrong machine unchanged.
pub fn is_loopback_url(raw: &str) -> bool {
    Url::parse(raw).ok().is_some_and(|url| loopback_host(&url).is_some())
}

struct LocalUrl {
    url: Url,
    destination: String,
    port: u16,
}

fn parse_local_url(raw: &str) -> Result<LocalUrl> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) {
        return Err(bad("잘못된 localhost URL이에요"));
    }
    let url = Url::parse(raw).map_err(|_| bad("URL을 읽을 수 없어요"))?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") {
        return Err(bad("HTTP·HTTPS·WS·WSS 주소만 연결할 수 있어요"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("로그인 정보가 들어간 URL은 연결하지 않아요"));
    }
    let destination = loopback_host(&url).ok_or_else(|| bad("원본 기기의 localhost 주소만 연결할 수 있어요"))?;
    let port = url.port_or_known_default().filter(|port| *port != 0)
        .ok_or_else(|| bad("유효한 서비스 포트가 필요해요"))?;
    Ok(LocalUrl { url, destination, port })
}

fn rewritten_url(local: &LocalUrl, port: u16) -> Result<String> {
    let mut url = local.url.clone();
    url.set_host(Some("127.0.0.1")).map_err(|_| bad("연결 주소를 만들 수 없어요"))?;
    url.set_port(Some(port)).map_err(|_| bad("연결 포트를 만들 수 없어요"))?;
    Ok(url.into())
}

struct Target {
    identity: String,
    ssh: String,
    key: Option<String>,
}

fn registered_target(request: &ResolveRequest) -> Result<Target> {
    let machine = if let Some(id) = request.source_machine_id.as_deref() {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(bad("원본 기기 ID가 잘못됐어요"));
        }
        crate::machines::find_route(&format!("~{id}"))
    } else {
        let mut matches = crate::machines::machines().into_iter()
            .filter(|machine| machine.label == request.source_machine);
        matches.next().filter(|_| matches.next().is_none())
    }.ok_or(RouteError(StatusCode::NOT_FOUND, "원본 기기를 이 기기의 명부에서 찾지 못했어요"))?;
    let ssh = machine.ssh.filter(|ssh| !ssh.is_empty()
        && !ssh.starts_with('-') && !ssh.chars().any(|c| c.is_whitespace() || c.is_control()))
        .ok_or_else(|| unavailable("원본 기기로 가는 등록된 SSH 경로가 없어요"))?;
    Ok(Target {
        identity: request.source_machine_id.clone().unwrap_or(machine.label),
        ssh, key: machine.key,
    })
}

#[derive(Hash, PartialEq, Eq)]
struct ForwardKey {
    identity: String,
    ssh: String,
    key: Option<String>,
    destination: String,
    port: u16,
}

struct Forward {
    child: Child,
    port: u16,
}

impl Drop for Forward {
    fn drop(&mut self) {
        // The Unix supervisor's TERM trap reaps only its own SSH child.
        #[cfg(unix)]
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM); }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        for _ in 0..30 {
            if self.child.try_wait().ok().flatten().is_some() { return; }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct ForwardPool { forwards: HashMap<ForwardKey, Forward> }

impl ForwardPool {
    fn resolve(
        &mut self, local: &LocalUrl, target: Target,
        launch: impl Fn(u16, &LocalUrl, &Target) -> std::io::Result<Child>,
    ) -> Result<ResolveResponse> {
        self.forwards.retain(|_, forward| matches!(forward.child.try_wait(), Ok(None)));
        let key = ForwardKey { identity: target.identity.clone(), ssh: target.ssh.clone(),
            key: target.key.clone(), destination: local.destination.clone(), port: local.port };
        if let Some(forward) = self.forwards.get_mut(&key) {
            let url = rewritten_url(local, forward.port)?;
            if probe_origin(&url).is_ok() && forward.child.try_wait().ok().flatten().is_none() {
                return Ok(ResolveResponse { ok: true, url, forwarded: true,
                    local_port: forward.port, source_port: local.port });
            }
            self.forwards.remove(&key);
        }
        if self.forwards.len() >= 32 {
            return Err(RouteError(StatusCode::CONFLICT, "브라우저 연결 포트가 너무 많아요. 앱을 다시 열어 정리해 주세요"));
        }
        // Never adopt an existing listener: it may be a different local service.
        let reservation = reserve_port(local.port)
            .map_err(|_| unavailable("이 기기에 빈 연결 포트를 잡지 못했어요"))?;
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let child = launch(port, local, &target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                unavailable("등록된 SSH 설정에 다른 포트 전달이 있거나 설정을 확인하지 못했어요. 추가 포워드 없는 기존 별칭이 필요해요")
            } else { unavailable("SSH 연결을 시작하지 못했어요") }
        })?;
        let mut forward = Forward { child, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        let address = (Ipv4Addr::LOCALHOST, port).into();
        loop {
            if forward.child.try_wait().ok().flatten().is_some() {
                return Err(unavailable("SSH 인증 또는 포트 전달이 거절됐어요. 기존 SSH 설정을 확인해 주세요"));
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() { break; }
            if Instant::now() >= deadline {
                return Err(unavailable("SSH 포트 연결을 기다리다 시간이 지났어요"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let url = rewritten_url(local, port)?;
        probe_origin(&url)?;
        // ExitOnForwardFailure prevents a bind race from silently adopting a
        // different service. Recheck the supervised process after the probe too.
        if forward.child.try_wait().ok().flatten().is_some() {
            return Err(unavailable("SSH 포트를 확보하지 못했어요"));
        }
        self.forwards.insert(key, forward);
        Ok(ResolveResponse { ok: true, url, forwarded: true, local_port: port, source_port: local.port })
    }
}

fn reserve_port(preferred: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, preferred))
        .or_else(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
}

fn probe_origin(raw: &str) -> Result<()> {
    let mut url = Url::parse(raw).map_err(|_| bad("연결 주소가 잘못됐어요"))?;
    let scheme = match url.scheme() { "ws" => "http", "wss" => "https", scheme => scheme }.to_owned();
    url.set_scheme(&scheme).map_err(|_| bad("연결 주소가 잘못됐어요"))?;
    url.set_path("/"); url.set_query(None); url.set_fragment(None);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|_| unavailable("연결 확인을 시작하지 못했어요"))?;
    runtime.block_on(async {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(4))
            .redirect(reqwest::redirect::Policy::none())
            // Probe transport only. The browser still enforces normal TLS trust;
            // this never changes browser/SSH certificate or host-key settings.
            .danger_accept_invalid_certs(true).no_proxy().build()
            .map_err(|_| unavailable("연결 확인을 시작하지 못했어요"))?;
        client.head(url).send().await
            .map(|_| ())
            .map_err(|_| unavailable("원본 localhost 서비스에 닿지 못했어요. 서비스 실행과 SSH 포트 전달 권한을 확인해 주세요"))
    })
}

fn ssh_arguments(port: u16, local: &LocalUrl, target: &Target) -> Vec<String> {
    let mut args: Vec<String> = ["-N", "-T", "-x", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes",
        "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=2",
        "-o", "ControlMaster=no", "-o", "ControlPath=none", "-o", "ForwardAgent=no",
        "-o", "ForkAfterAuthentication=no", "-o", "PermitLocalCommand=no", "-o", "Tunnel=no"]
        .into_iter().map(str::to_string).collect();
    if let Some(key) = &target.key { args.extend(["-i".into(), key.clone()]); }
    args.extend(["-L".into(), format!("127.0.0.1:{port}:{}:{}", local.destination, local.port), target.ssh.clone()]);
    args
}

#[cfg(unix)]
fn launch_ssh(port: u16, local: &LocalUrl, target: &Target) -> std::io::Result<Child> {
    check_inherited_forwards(&target.ssh)?;
    let parent = std::process::id().to_string();
    crate::no_window_command("sh").args(["-c",
        "owner=$1; shift; child=''; monitor=''; cleanup() { if [ -n \"$child\" ]; then kill \"$child\" 2>/dev/null; wait \"$child\" 2>/dev/null; fi; if [ -n \"$monitor\" ]; then kill \"$monitor\" 2>/dev/null; wait \"$monitor\" 2>/dev/null; fi; }; trap 'cleanup; exit 0' TERM INT; ssh \"$@\" & child=$!; (while kill -0 \"$owner\" 2>/dev/null; do sleep 1; done; kill \"$child\" 2>/dev/null) & monitor=$!; wait \"$child\"; result=$?; kill \"$monitor\" 2>/dev/null; wait \"$monitor\" 2>/dev/null; exit \"$result\"",
        "kasaterm-browser-forward", &parent])
        .args(ssh_arguments(port, local, target))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}

fn inherited_forward(line: &str) -> bool {
    matches!(line.split_whitespace().next(), Some("localforward" | "remoteforward" | "dynamicforward"))
}

/// -L adds to forwards in ssh_config. ClearAllForwardings would also remove our
/// own -L, so reject inherited forwards rather than launching unrelated sockets
/// or replacing the user's SSH configuration. -G does not connect to the host.
#[cfg(unix)]
fn check_inherited_forwards(target: &str) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut child = crate::no_window_command("ssh")
        .args(["-G", "-T", "-o", "PermitLocalCommand=no", target])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()?;
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut forwards = false;
        for line in std::io::BufReader::new(stdout).lines() {
            match line { Ok(line) => forwards |= inherited_forward(&line), Err(_) => { forwards = true; break; } }
        }
        let _ = sender.send(forwards);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait()? {
            Some(status) => {
                let foreign = receiver.recv_timeout(Duration::from_secs(1)).unwrap_or(true);
                return if status.success() && !foreign { Ok(()) } else {
                    Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "SSH config contains additional forwards or cannot be checked"))
                };
            }
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            None => {
                let _ = child.kill(); let _ = child.wait();
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "SSH config inspection timed out"));
            }
        }
    }
}

#[cfg(not(unix))]
fn launch_ssh(_: u16, _: &LocalUrl, _: &Target) -> std::io::Result<Child> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "SSH forwarding supervisor is unavailable"))
}

pub(crate) async fn resolve_handler(Json(request): Json<ResolveRequest>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        let local = parse_local_url(&request.url)?;
        if request.source_machine_id.as_deref().is_some_and(|id| {
            crate::mobile::machine_identity().as_deref() == Some(id)
        }) {
            return Ok(ResolveResponse { ok: true, url: request.url, forwarded: false,
                local_port: local.port, source_port: local.port });
        }
        let target = registered_target(&request)?;
        static POOL: OnceLock<Mutex<ForwardPool>> = OnceLock::new();
        let mut pool = POOL.get_or_init(Default::default).try_lock()
            .map_err(|_| RouteError(StatusCode::CONFLICT, "다른 브라우저 연결을 준비 중이에요. 잠시 뒤 다시 열어 주세요"))?;
        pool.resolve(&local, target, launch_ssh)
    }).await;
    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(serde_json::to_value(response).unwrap())),
        Ok(Err(RouteError(status, error))) => (status, Json(serde_json::json!({"ok":false,"error":error}))),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"ok":false,"error":"연결 확인이 중단됐어요"}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn only_loopback_web_urls_are_forwardable() {
        for url in ["http://localhost:3000", "https://127.0.0.1", "http://127.0.0.2:3000", "ws://[::1]:8080/ws", "wss://localhost./ws"] {
            assert!(is_loopback_url(url)); assert!(parse_local_url(url).is_ok());
        }
        for url in ["http://example.com:3000", "http://localhost.example.com", "http://192.168.1.1", "file:///tmp/a", "ftp://localhost/a", "http://user:pass@localhost:3000", "http://localhost:0"] {
            assert!(parse_local_url(url).is_err(), "accepted {url}");
        }
        assert!(is_loopback_url("ftp://localhost/file"));
    }
    #[test]
    fn preserves_url_components_and_websocket_scheme() {
        let local = parse_local_url("ws://localhost:3000/a%20b?x=%2F#frag").unwrap();
        assert_eq!(rewritten_url(&local, 4000).unwrap(), "ws://127.0.0.1:4000/a%20b?x=%2F#frag");
    }
    #[test]
    fn port_collision_does_not_adopt_an_existing_listener() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = occupied.local_addr().unwrap().port();
        assert_ne!(reserve_port(port).unwrap().local_addr().unwrap().port(), port);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok());
    }
    #[test]
    fn ssh_has_one_literal_loopback_forward_and_no_remote_command() {
        let local = parse_local_url("http://[::1]:3000/test").unwrap();
        let target = Target { identity:"test".into(), ssh:"registered-alias".into(), key:None };
        let args = ssh_arguments(45000, &local, &target);
        assert_eq!(&args[args.len()-3..], ["-L", "127.0.0.1:45000:[::1]:3000", "registered-alias"]);
        assert!(args.iter().any(|arg| arg=="-N"));
        assert!(!args.iter().any(|arg| arg.contains("StrictHostKeyChecking=no")));
        assert!(inherited_forward("localforward 1234 remote.example:80"));
        assert!(inherited_forward("remoteforward 5678 local.example:80"));
        assert!(!inherited_forward("hostname registered.example"));
    }

    // An isolated TCP/SSH-process fixture: the child substitutes only the SSH
    // transport, while the production pool, port allocation, readiness probe,
    // URL rewrite and lifecycle cleanup are exercised unchanged.
    #[test]
    fn forward_fixture_child() {
        let Ok(spec) = std::env::var("KASATERM_BROWSER_ROUTE_FIXTURE") else { return };
        let (local, remote) = spec.split_once(':').unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local.parse::<u16>().unwrap())).unwrap();
        let remote = remote.parse::<u16>().unwrap();
        for incoming in listener.incoming() {
            let Ok(mut incoming) = incoming else { break };
            std::thread::spawn(move || {
                let Ok(mut outgoing) = TcpStream::connect((Ipv4Addr::LOCALHOST, remote)) else { return };
                let mut incoming_read = incoming.try_clone().unwrap();
                let mut outgoing_write = outgoing.try_clone().unwrap();
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut incoming_read, &mut outgoing_write);
                    let _ = outgoing_write.shutdown(std::net::Shutdown::Write);
                });
                let _ = std::io::copy(&mut outgoing, &mut incoming);
                let _ = incoming.shutdown(std::net::Shutdown::Write);
            });
        }
    }

    struct WebFixture {
        port: u16,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl WebFixture {
        fn start() -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            let worker = std::thread::spawn(move || {
                while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(Duration::from_millis(5)); continue;
                    };
                    std::thread::spawn(move || {
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                            let mut byte = [0u8; 1];
                            if socket.read(&mut byte).unwrap_or(0) == 0 { return; }
                            request.push(byte[0]);
                        }
                        if request.starts_with(b"GET /ws ") {
                            let _ = socket.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n\x81\x05hello");
                        } else if request.starts_with(b"HEAD ") {
                            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\n");
                        } else {
                            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture");
                        }
                    });
                }
            });
            Self { port, stop, worker: Some(worker) }
        }
    }
    impl Drop for WebFixture {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(worker) = self.worker.take() { let _ = worker.join(); }
        }
    }

    fn fixture_child(local: u16, remote: u16) -> std::io::Result<Child> {
        std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "browser_route::tests::forward_fixture_child", "--nocapture"])
            .env("KASATERM_BROWSER_ROUTE_FIXTURE", format!("{local}:{remote}"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    }
    fn fixture_target() -> Target {
        Target { identity:"isolated-fixture".into(), ssh:"fixture-only".into(), key:None }
    }
    fn request(port: u16, bytes: &[u8]) -> Vec<u8> {
        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        socket.write_all(bytes).unwrap();
        let mut response = Vec::new(); socket.read_to_end(&mut response).unwrap(); response
    }

    #[test]
    fn fixture_routes_http_and_websocket_reuses_forward_and_releases_port() {
        let source = WebFixture::start();
        let local = parse_local_url(&format!("http://localhost:{}/page?x=1#frag", source.port)).unwrap();
        let mut pool = ForwardPool::default();
        let response = pool.resolve(&local, fixture_target(), |port, _, _| fixture_child(port, source.port)).unwrap();
        assert_ne!(response.local_port, source.port, "source fixture occupies preferred local port");
        assert!(response.url.ends_with("/page?x=1#frag"));
        assert!(request(response.local_port, b"GET /page HTTP/1.1\r\nHost: localhost\r\n\r\n").ends_with(b"fixture"));
        let websocket = parse_local_url(&format!("ws://localhost:{}/ws", source.port)).unwrap();
        let reused = pool.resolve(&websocket, fixture_target(), |_, _, _| panic!("must reuse TCP tunnel")).unwrap();
        assert_eq!(reused.local_port, response.local_port);
        assert!(reused.url.starts_with("ws://127.0.0.1:"));
        let frame = request(reused.local_port, b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n");
        assert!(frame.starts_with(b"HTTP/1.1 101"));
        assert!(frame.ends_with(b"\x81\x05hello"));
        let port = response.local_port;
        drop(pool);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
        assert!(request(source.port, b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").ends_with(b"fixture"));
    }

    #[test]
    fn fixture_prefers_same_port_when_unused() {
        let source = WebFixture::start();
        let reservation = reserve_port(0).unwrap();
        let preferred = reservation.local_addr().unwrap().port();
        drop(reservation);
        let local = parse_local_url(&format!("http://127.0.0.1:{preferred}/")).unwrap();
        let mut pool = ForwardPool::default();
        let response = pool.resolve(&local, fixture_target(), |port, _, _| fixture_child(port, source.port)).unwrap();
        assert_eq!(response.local_port, preferred);
    }

    #[cfg(unix)]
    #[test]
    fn failed_forward_returns_error_and_does_not_reserve_an_origin() {
        let local = parse_local_url("http://localhost:39871/").unwrap();
        let mut pool = ForwardPool::default();
        let result = pool.resolve(&local, fixture_target(), |_, _, _| std::process::Command::new("false").spawn());
        assert!(result.is_err());
        assert!(pool.forwards.is_empty());
    }
}
