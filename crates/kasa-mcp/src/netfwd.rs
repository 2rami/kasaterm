//! 카사넷 P3 — 다른 기기의 포트를 이 기기 localhost 로. 설계는 `docs/kasanet.md` 「P3 포트 공유」.
//!
//! 받는 쪽: 앱 HTTP 서버의 `/net/tcp?port=N` 웹소켓이 `127.0.0.1:N` 과 바이트를 잇는다. 문은 다른 라우트와
//! 같은 `origin_guard_mw`(루프백 peer 는 그대로, 원격은 토큰)에 더해 교차 출처 웹소켓과 손님 폰 주소를 막는다.
//! 끌어오는 쪽: `127.0.0.1:L`(+`[::1]:L`)을 듣고 연결마다 `route_base(기기 base)/net/tcp` 를 연다 — P2 입구가
//! 직통이면 카사넷, 아니면 원래 base(ssh 터널)로 고르므로 폴백이 따로 없다.
//!
//! 틀: 바이너리 프레임 = 날 바이트. 텍스트 프레임 `eof` = 보낸 쪽 TCP 가 쓰기를 닫았다(반쯤 닫기) — 요청을
//! 다 쓰고 쓰기를 닫은 뒤 응답을 기다리는 클라이언트가 있어 한쪽 끝을 전체 닫기로 바꾸면 응답이 잘린다.
//! 그 밖의 텍스트는 무시하고, 웹소켓 Close·끊김은 전체 닫기다.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::ws::{Message as AxMessage, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// 반쯤 닫기 표시. 폰(P5)도 같은 글자를 쓴다.
const EOF_TEXT: &str = "eof";
/// 관문(cloudflared) 무료 플랜은 유휴 웹소켓을 ~100초에 끊는다. 개발 서버의 HMR 소켓은 오래 조용하다.
const PING_EVERY: Duration = Duration::from_secs(30);
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 한 앱이 드는 끌어오기 상한. 보여 주기는 같은 기기·포트를 다시 쓰므로 부를 때마다 늘지 않는다.
const MAX_FORWARDS: usize = 32;

#[derive(Debug)]
enum Frame {
    Data(Bytes),
    Eof,
    Ping,
    Close,
    Skip,
}

fn from_axum(m: Result<AxMessage, axum::Error>) -> Frame {
    match m {
        Ok(AxMessage::Binary(b)) => Frame::Data(b),
        Ok(AxMessage::Text(t)) if t.as_str() == EOF_TEXT => Frame::Eof,
        Ok(AxMessage::Close(_)) | Err(_) => Frame::Close,
        Ok(_) => Frame::Skip,
    }
}

fn to_axum(f: Frame) -> AxMessage {
    match f {
        Frame::Data(b) => AxMessage::Binary(b),
        Frame::Eof => AxMessage::Text(EOF_TEXT.into()),
        Frame::Ping => AxMessage::Ping(Bytes::new()),
        Frame::Close | Frame::Skip => AxMessage::Close(None),
    }
}

fn from_ws(m: Result<WsMessage, tokio_tungstenite::tungstenite::Error>) -> Frame {
    match m {
        Ok(WsMessage::Binary(b)) => Frame::Data(b),
        Ok(WsMessage::Text(t)) if t.as_str() == EOF_TEXT => Frame::Eof,
        Ok(WsMessage::Close(_)) | Err(_) => Frame::Close,
        Ok(_) => Frame::Skip,
    }
}

fn to_ws(f: Frame) -> WsMessage {
    match f {
        Frame::Data(b) => WsMessage::Binary(b),
        Frame::Eof => WsMessage::Text(EOF_TEXT.into()),
        Frame::Ping => WsMessage::Ping(Bytes::new()),
        Frame::Close | Frame::Skip => WsMessage::Close(None),
    }
}

/// TCP 하나와 웹소켓 하나를 잇는다. 두 방향은 따로 돈다 — 한쪽 쓰기가 막힌 동안 다른 쪽 읽기까지 멈추면
/// 서로의 받는 버퍼가 차 둘 다 멈춘다. 양쪽이 다 `eof` 를 냈을 때만 정상 종료(Close)다.
async fn pump<R, W>(mut rx: R, tx: W, tcp: TcpStream, ping: Option<Duration>)
where
    R: Stream<Item = Frame> + Unpin,
    W: Sink<Frame> + Unpin,
{
    let _ = tcp.set_nodelay(true);
    let (mut rd, mut wr) = tcp.into_split();
    let tx = tokio::sync::Mutex::new(tx);
    let up = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let frame = match rd.read(&mut buf).await {
                Ok(0) | Err(_) => Frame::Eof,
                Ok(n) => Frame::Data(Bytes::copy_from_slice(&buf[..n])),
            };
            let eof = matches!(frame, Frame::Eof);
            if tx.lock().await.send(frame).await.is_err() {
                return false;
            }
            if eof {
                return true;
            }
        }
    };
    let down = async {
        loop {
            match rx.next().await {
                Some(Frame::Data(b)) => {
                    if wr.write_all(&b).await.is_err() {
                        return false;
                    }
                }
                Some(Frame::Eof) => {
                    let _ = wr.shutdown().await;
                    return true;
                }
                Some(Frame::Skip | Frame::Ping) => {}
                Some(Frame::Close) | None => return false,
            }
        }
    };
    let pinger = async {
        let Some(every) = ping else {
            return std::future::pending::<()>().await;
        };
        loop {
            tokio::time::sleep(every).await;
            if tx.lock().await.send(Frame::Ping).await.is_err() {
                return;
            }
        }
    };
    tokio::pin!(up, down, pinger);
    let (mut up_done, mut down_done) = (false, false);
    while !(up_done && down_done) {
        tokio::select! {
            ok = &mut up, if !up_done => if ok { up_done = true } else { return },
            ok = &mut down, if !down_done => if ok { down_done = true } else { break },
            _ = &mut pinger => return,
        }
    }
    let mut tx = tx.lock().await;
    let _ = tx.send(Frame::Close).await;
    let _ = tx.close().await;
}

// ── 받는 쪽: /net/tcp ─────────────────────────────────────────────────────────

fn port_param(query: Option<&str>) -> Option<u16> {
    query?
        .split('&')
        .find_map(|kv| kv.strip_prefix("port="))
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
}

/// 루프백만. 개발 서버가 `localhost` 로 뜨면 `::1` 에만 묶이는 일이 있어(Node 17+ 의 Vite) 거부되면 한 번 더.
async fn connect_loopback(port: u16) -> std::io::Result<TcpStream> {
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let first = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(v4)).await;
    match first {
        Ok(Ok(s)) => Ok(s),
        _ => tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(v6))
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "loopback connect timed out"))?,
    }
}

/// `GET /net/tcp?port=N` — 이 기기 `127.0.0.1:N` 과 잇는 웹소켓. 원격 토큰은 `origin_guard_mw` 가 이미 봤다.
pub(crate) async fn tcp_ws_handler(req: axum::extract::Request) -> Response {
    // 폰 주소로 들어오면 이 기기의 아무 포트에 닿는 로컬 권한이 된다 — 주인만.
    if let Some(denied) = crate::http::guest_denied(&req) {
        return denied;
    }
    if !crate::http::ws_origin_ok(req.headers()) {
        return (axum::http::StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response();
    }
    let Some(port) = port_param(req.uri().query()) else {
        return (axum::http::StatusCode::BAD_REQUEST, "port=1..65535 required").into_response();
    };
    use axum::extract::FromRequestParts as _;
    let (mut parts, _body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    // 업그레이드 전에 붙여 본다 — 끌어오는 쪽이 「아무도 안 듣는다」를 101 뒤 끊김이 아니라 502 로 받게.
    let tcp = match connect_loopback(port).await {
        Ok(s) => s,
        Err(_) => {
            return (axum::http::StatusCode::BAD_GATEWAY, format!("nothing listening on 127.0.0.1:{port}")).into_response();
        }
    };
    ws.on_upgrade(move |socket| async move {
        let (tx, rx) = socket.split();
        let tx = tx.with(|f: Frame| std::future::ready(Ok::<_, axum::Error>(to_axum(f))));
        pump(rx.map(from_axum), Box::pin(tx), tcp, Some(PING_EVERY)).await;
    })
}

// ── 끌어오는 쪽: net forward ──────────────────────────────────────────────────

/// 끌어올 상대. 명부 이름이 있으면 연결마다 base 를 다시 찾는다 — 기기 길(자체 터널·손님 -R)이 바뀌어도 따라간다.
#[derive(Clone)]
struct Target {
    label: String,
    machine: Option<String>,
    base: String,
    port: u16,
}

impl Target {
    fn current_base(&self) -> String {
        self.machine
            .as_deref()
            .and_then(crate::machines::find_route)
            .map(|m| m.base)
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| self.base.clone())
    }
}

struct Forward {
    target: Target,
    local: u16,
    v6: bool,
    by: &'static str,
    active: Arc<AtomicUsize>,
    total: Arc<AtomicU64>,
    stop: tokio::sync::watch::Sender<bool>,
    listeners: Vec<tokio::task::JoinHandle<()>>,
}

fn forwards() -> &'static Mutex<BTreeMap<u16, Forward>> {
    static F: OnceLock<Mutex<BTreeMap<u16, Forward>>> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// 끌어오기는 앱 어디서 불려도(소켓 스레드·GUI 스레드·HTTP 처리기) 같은 런타임에서 산다.
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kasa-netfwd")
            .enable_all()
            .build()
            .expect("netfwd runtime")
    })
}

fn blocking<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    rt().spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv_timeout(Duration::from_secs(30)).ok()
}

fn same_base(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

fn ws_base(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    }
}

type Dialed = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// 상대 `/net/tcp` 하나. 기기 base 대신 P2 입구(`route_base`)로 간다 — 입구가 직통이면 카사넷, 아니면 원래 길.
async fn dial(base: &str, port: u16) -> Result<Dialed, String> {
    let entry = crate::kasanet::route_base(base);
    let mut req = format!("{}/net/tcp?port={port}", ws_base(&entry))
        .into_client_request()
        .map_err(|e| format!("주소를 못 만들었어요: {e}"))?;
    if let Some(token) = crate::remote::connection_auth_token(base).and_then(|t| t.parse().ok()) {
        req.headers_mut().insert("x-kasa-token", token);
    }
    let dialed = tokio::time::timeout(DIAL_TIMEOUT, tokio_tungstenite::connect_async_with_config(req, None, true)).await;
    match dialed {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(r))) => Err(match r.status().as_u16() {
            404 => "상대 카사텀이 옛 판이라 포트 공유를 몰라요".to_string(),
            502 => format!("상대 기기 {port} 포트에 듣는 게 없어요"),
            403 => "상대 기기가 거부했어요(원격 토큰)".to_string(),
            s => format!("상대 기기가 HTTP {s} 로 답했어요"),
        }),
        Ok(Err(e)) => Err(format!("상대 기기에 못 닿았어요: {e}")),
        Err(_) => Err("상대 기기가 10초 안에 답하지 않았어요".to_string()),
    }
}

struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn carry(tcp: TcpStream, target: Arc<Target>, mut stop: tokio::sync::watch::Receiver<bool>, active: Arc<AtomicUsize>) {
    active.fetch_add(1, Ordering::Relaxed);
    let _counted = Counted(active);
    let base = target.current_base();
    let ws = tokio::select! {
        ws = dial(&base, target.port) => ws,
        _ = stop.changed() => return,
    };
    let ws = match ws {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[netfwd] {}:{} {e}", target.label, target.port);
            return;
        }
    };
    let (tx, rx) = ws.split();
    let tx = tx.with(|f: Frame| std::future::ready(Ok::<_, tokio_tungstenite::tungstenite::Error>(to_ws(f))));
    tokio::select! {
        _ = pump(rx.map(from_ws), Box::pin(tx), tcp, None) => {}
        _ = stop.changed() => {}
    }
}

async fn accept_loop(
    listener: tokio::net::TcpListener,
    target: Arc<Target>,
    stop: tokio::sync::watch::Receiver<bool>,
    active: Arc<AtomicUsize>,
    total: Arc<AtomicU64>,
) {
    let mut stopped = stop.clone();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((tcp, _)) = accepted else { continue };
                total.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(carry(tcp, target.clone(), stop.clone(), active.clone()));
            }
            _ = stopped.changed() => return,
        }
    }
}

/// 이 기기에서 누가 듣고 있나 — macOS 는 `0.0.0.0:P` 로 뜬 개발 서버가 있어도 `127.0.0.1:P` 를 묶게 해 줘서
/// 묶기 성공만 보면 그 서버를 가로챈다.
fn someone_listens(port: u16) -> bool {
    [SocketAddr::from((Ipv4Addr::LOCALHOST, port)), SocketAddr::from((Ipv6Addr::LOCALHOST, port))]
        .iter()
        .any(|a| std::net::TcpStream::connect_timeout(a, Duration::from_millis(200)).is_ok())
}

/// `127.0.0.1:L` 과 `[::1]:L` 을 같이 잡는다. v4 만 잡으면 `localhost` 가 `::1` 로 풀리는 브라우저가 그 자리의
/// 다른 서비스로 간다. `::1` 이 없는 기기는 v4 만.
fn bind_pair(port: u16) -> std::io::Result<(std::net::TcpListener, Option<std::net::TcpListener>)> {
    let v4 = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
    let port = v4.local_addr()?.port();
    match std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(v6) => Ok((v4, Some(v6))),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Err(e),
        Err(_) => Ok((v4, None)),
    }
}

/// 원하는 번호가 비었으면 그 번호(주소가 원래 것과 같아 OAuth 되돌아오기·쿠키가 그대로), 아니면 빈 번호.
/// `--local` 로 못박은 번호는 비어 있지 않으면 오류다.
fn bind_local(preferred: u16, strict: bool) -> Result<(std::net::TcpListener, Option<std::net::TcpListener>), String> {
    let busy = || format!("이 기기 {preferred} 포트를 이미 누가 쓰고 있어요");
    if !someone_listens(preferred) {
        match bind_pair(preferred) {
            Ok(pair) => return Ok(pair),
            Err(_) if strict => return Err(busy()),
            Err(_) => {}
        }
    } else if strict {
        return Err(busy());
    }
    (0..8)
        .find_map(|_| bind_pair(0).ok())
        .ok_or_else(|| "이 기기에 빈 포트를 못 잡았어요".to_string())
}

struct Opened {
    local: u16,
    v6: bool,
    reused: bool,
}

async fn open(target: Target, local: Option<u16>, by: &'static str) -> Result<Opened, String> {
    // 같은 상대를 동시에 두 번 열면 둘 다 새로 묶는다 — 여는 일은 한 번에 하나.
    static GATE: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    let _gate = GATE.get_or_init(Default::default).lock().await;
    {
        let list = forwards().lock().map_err(|_| "끌어오기 목록이 잠겼어요".to_string())?;
        if local.is_none() {
            if let Some(f) = list.values().find(|f| same_base(&f.target.base, &target.base) && f.target.port == target.port) {
                return Ok(Opened { local: f.local, v6: f.v6, reused: true });
            }
        }
        if let Some(l) = local.filter(|l| list.contains_key(l)) {
            return Err(format!("이미 localhost:{l} 로 끌어오는 중이에요 — net stop {l} 먼저"));
        }
        if list.len() >= MAX_FORWARDS {
            return Err(format!("끌어오기가 {MAX_FORWARDS}개예요 — net stop 으로 닫고 다시"));
        }
    }
    // 묶기 전에 한 번 붙어 본다 — 옛 판·빈 포트·거부를 여기서 말하고, 실패한 끌어오기가 포트를 쥐고 남지 않게.
    let mut probe = dial(&target.base, target.port).await?;
    let _ = probe.close(None).await;
    let (v4, v6) = bind_local(local.unwrap_or(target.port), local.is_some())?;
    let port = v4.local_addr().map_err(|e| e.to_string())?.port();
    let has_v6 = v6.is_some();
    let (stop, stop_rx) = tokio::sync::watch::channel(false);
    let active = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let shared = Arc::new(target.clone());
    let mut listeners = Vec::new();
    for l in std::iter::once(v4).chain(v6) {
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let l = tokio::net::TcpListener::from_std(l).map_err(|e| e.to_string())?;
        listeners.push(tokio::spawn(accept_loop(l, shared.clone(), stop_rx.clone(), active.clone(), total.clone())));
    }
    eprintln!("[netfwd] localhost:{port} ← {}:{} ({by})", target.label, target.port);
    forwards().lock().map_err(|_| "끌어오기 목록이 잠겼어요".to_string())?.insert(
        port,
        Forward { target, local: port, v6: has_v6, by, active, total, stop, listeners },
    );
    Ok(Opened { local: port, v6: has_v6, reused: false })
}

async fn stop(local: u16) -> Result<Target, String> {
    let f = forwards()
        .lock()
        .map_err(|_| "끌어오기 목록이 잠겼어요".to_string())?
        .remove(&local)
        .ok_or_else(|| format!("localhost:{local} 로 끌어오는 것이 없어요"))?;
    let _ = f.stop.send(true);
    // 듣던 소켓이 실제로 닫힌 뒤 답한다 — 곧바로 친 요청이 닫히는 중인 끌어오기에 붙지 않게.
    for l in f.listeners {
        let _ = l.await;
    }
    eprintln!("[netfwd] localhost:{local} 닫음");
    Ok(f.target)
}

fn path_text(base: &str) -> (&'static str, Option<u64>) {
    crate::kasanet::path(base).unwrap_or(("base", None))
}

fn row(f: &Forward) -> Value {
    let base = f.target.current_base();
    let (path, rtt) = path_text(&base);
    json!({
        "local": f.local,
        "url": format!("http://localhost:{}", f.local),
        "machine": f.target.label,
        "port": f.target.port,
        "path": path,
        "path_rtt_ms": rtt,
        "active": f.active.load(Ordering::Relaxed),
        "total": f.total.load(Ordering::Relaxed),
        "by": f.by,
    })
}

fn path_label(path: &str, rtt: Option<u64>) -> String {
    match (path, rtt) {
        ("kasanet", Some(ms)) => format!("카사넷 직통 {ms}ms"),
        ("kasanet-relay", Some(ms)) => format!("카사넷 자체 중계 {ms}ms"),
        ("ssh", _) => "ssh 길".to_string(),
        _ => "기기 주소로 곧장".to_string(),
    }
}

fn machine_target(name: &str, port: u16) -> Result<Target, String> {
    let m = crate::machines::find_route(name).ok_or_else(|| format!("명부에 「{name}」 기기가 없어요"))?;
    if m.base.trim().is_empty() {
        return Err(format!("「{name}」 로 가는 길이 아직 없어요(터널이 서는 중이거나 꺼져 있음)"));
    }
    Ok(Target { label: m.label.clone(), machine: Some(name.to_string()), base: m.base, port })
}

/// 소켓 `net.forward` — CLI `net forward|list|stop`.
pub fn handle(params: &Value) -> anyhow::Result<Value> {
    let port_of = |k: &str| {
        params.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0)
    };
    match params.get("op").and_then(Value::as_str).unwrap_or("list") {
        "forward" => {
            let name = params.get("machine").and_then(Value::as_str).unwrap_or_default();
            let port = port_of("port").ok_or_else(|| anyhow::anyhow!("net forward <기기> <port> [--local L]"))?;
            let local = port_of("local");
            let target = machine_target(name, port).map_err(anyhow::Error::msg)?;
            let label = target.label.clone();
            let base = target.base.clone();
            let opened = blocking(open(target, local, "cli"))
                .ok_or_else(|| anyhow::anyhow!("끌어오기가 30초 안에 끝나지 않았어요"))?
                .map_err(anyhow::Error::msg)?;
            let (path, rtt) = path_text(&base);
            let url = format!("http://localhost:{}", opened.local);
            let again = if opened.reused { " · 이미 열려 있던 것" } else { "" };
            Ok(json!({
                "ok": true, "local": opened.local, "url": url, "machine": label, "port": port,
                "reused": opened.reused, "v6": opened.v6, "path": path, "path_rtt_ms": rtt,
                "summary": format!("{url} ← {label}:{port} ({}){again}", path_label(path, rtt)),
            }))
        }
        "list" => {
            let rows: Vec<Value> = forwards().lock().map_err(|_| anyhow::anyhow!("끌어오기 목록이 잠겼어요"))?.values().map(row).collect();
            let summary = if rows.is_empty() {
                "끌어오는 포트 없음".to_string()
            } else {
                rows.iter()
                    .map(|r| {
                        format!(
                            "localhost:{} ← {}:{} ({}) 연결 {}/{}",
                            r["local"], r["machine"].as_str().unwrap_or_default(), r["port"],
                            path_label(r["path"].as_str().unwrap_or_default(), r["path_rtt_ms"].as_u64()),
                            r["active"], r["total"],
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(json!({ "ok": true, "forwards": rows, "summary": summary }))
        }
        "stop" => {
            let local = port_of("local").ok_or_else(|| anyhow::anyhow!("net stop <로컬 포트>"))?;
            let target = blocking(stop(local))
                .ok_or_else(|| anyhow::anyhow!("닫기가 30초 안에 끝나지 않았어요"))?
                .map_err(anyhow::Error::msg)?;
            Ok(json!({
                "ok": true, "local": local,
                "summary": format!("localhost:{local} 닫음 ({}:{})", target.label, target.port),
            }))
        }
        other => anyhow::bail!("net forward|list|stop — 모르는 것: {other}"),
    }
}

// ── 보여 주기: 다른 기기의 localhost 주소를 이 기기에서 열기 ────────────────────

/// 끌어올 수 있는 주소면 (주소, 포트). http·https·ws·wss 의 루프백 호스트만.
fn loopback_url(raw: &str) -> Option<(reqwest::Url, u16)> {
    let url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https" | "ws" | "wss") || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    let loopback = matches!(host, "localhost" | "localhost.")
        || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    let port = url.port_or_known_default().filter(|p| *p != 0)?;
    loopback.then_some((url, port))
}

/// 호스트는 되도록 원래 모양 그대로(쿠키·OAuth 되돌아오기 주소가 호스트까지 맞춘다), 포트만 바꾼다.
fn rewrite(mut url: reqwest::Url, local: u16, v6: bool) -> String {
    let keep = match url.host_str().map(|h| h.trim_matches(['[', ']'])) {
        Some("localhost" | "127.0.0.1") => true,
        Some("::1") => v6,
        _ => false,
    };
    if !keep {
        let _ = url.set_host(Some("localhost"));
    }
    let _ = url.set_port(Some(local));
    url.into()
}

async fn show(target: Target, url: reqwest::Url) -> Result<(String, u16), String> {
    let opened = open(target, None, "show").await?;
    Ok((rewrite(url, opened.local, opened.v6), opened.local))
}

/// 거울이 받은 원본 기기의 `open-url` — 그 주소가 원본의 localhost 면 포트를 끌어와 이 기기 주소로 바꾼다.
/// 끌어올 게 아니면 None, 못 끌어오면 오류(부른 쪽이 원래 주소로 열지 정한다).
pub async fn show_from_base(base: &str, raw: &str) -> Option<Result<String, String>> {
    let (url, port) = loopback_url(raw)?;
    let label = crate::machines::label_for_base(base).unwrap_or_else(|| base.to_string());
    let target = Target { label, machine: None, base: base.to_string(), port };
    Some(match rt().spawn(show(target, url)).await {
        Ok(r) => r.map(|(u, _)| u),
        Err(e) => Err(e.to_string()),
    })
}

/// 브라우저 기기(`/browser/resolve-localhost`)가 원본 기기의 localhost 를 끌어온다. (열 주소, 로컬 포트, 원본 포트).
pub fn show_from_machine_blocking(machine: &crate::machines::Machine, raw: &str) -> Result<(String, u16, u16), String> {
    let (url, port) = loopback_url(raw).ok_or_else(|| "localhost 주소가 아니에요".to_string())?;
    if machine.base.trim().is_empty() {
        return Err("원본 기기로 가는 길이 없어요".to_string());
    }
    let target = Target { label: machine.label.clone(), machine: Some(machine.label.clone()), base: machine.base.clone(), port };
    let (shown, local) = blocking(show(target, url)).ok_or_else(|| "끌어오기가 30초 안에 끝나지 않았어요".to_string())??;
    Ok((shown, local, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                    let _ = w.shutdown().await;
                });
            }
        });
        port
    }

    async fn app(layer: Option<crate::mobile::MobileUser>) -> String {
        let mut router = axum::Router::new().route("/net/tcp", axum::routing::get(tcp_ws_handler));
        if let Some(user) = layer {
            router = router.layer(axum::middleware::from_fn(move |mut r: axum::extract::Request, next: axum::middleware::Next| {
                r.extensions_mut().insert(crate::http::MobileAuth(user.clone()));
                async move { next.run(r).await }
            }));
        }
        let router = router.layer(axum::middleware::from_fn(crate::http::origin_guard_mw));
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("ws://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(l, router.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
        });
        base
    }

    fn status_of(e: tokio_tungstenite::tungstenite::Error) -> u16 {
        match e {
            tokio_tungstenite::tungstenite::Error::Http(r) => r.status().as_u16(),
            other => panic!("HTTP 거부가 아니다: {other}"),
        }
    }

    async fn next_data<S>(ws: &mut S) -> Frame
    where
        S: Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("응답 없음") {
                Some(m) => match from_ws(m) {
                    Frame::Skip | Frame::Ping => continue,
                    f => return f,
                },
                None => return Frame::Close,
            }
        }
    }

    #[tokio::test]
    async fn bytes_round_trip_and_half_close() {
        let echo = echo_server().await;
        let base = app(None).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.unwrap();
        ws.send(to_ws(Frame::Data(Bytes::from_static(b"hello")))).await.unwrap();
        assert!(matches!(next_data(&mut ws).await, Frame::Data(b) if &b[..] == b"hello"));
        // 쓰기를 닫아도 받는 쪽은 살아 있어야 한다 — 에코 서버는 EOF 를 보고서야 제 쓰기를 닫는다.
        ws.send(to_ws(Frame::Data(Bytes::from_static(b"tail")))).await.unwrap();
        ws.send(to_ws(Frame::Eof)).await.unwrap();
        assert!(matches!(next_data(&mut ws).await, Frame::Data(b) if &b[..] == b"tail"));
        assert!(matches!(next_data(&mut ws).await, Frame::Eof));
        assert!(matches!(next_data(&mut ws).await, Frame::Close));
    }

    #[tokio::test]
    async fn refuses_before_upgrade() {
        let base = app(None).await;
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={dead}")).await.unwrap_err();
        assert_eq!(status_of(e), 502);
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port=0")).await.unwrap_err();
        assert_eq!(status_of(e), 400);
    }

    #[tokio::test]
    async fn remote_needs_token_and_page_origin() {
        let echo = echo_server().await;
        let base = app(None).await;
        let url = format!("{base}/net/tcp?port={echo}");
        // 터널로 들어온 요청은 peer 가 루프백이어도 원격이다.
        let mut r = url.clone().into_client_request().unwrap();
        r.headers_mut().insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(status_of(tokio_tungstenite::connect_async(r).await.unwrap_err()), 403);
        // 남의 페이지가 여는 웹소켓 — 루프백이라도 막는다.
        let mut r = url.clone().into_client_request().unwrap();
        r.headers_mut().insert("origin", "https://evil.example.com".parse().unwrap());
        assert_eq!(status_of(tokio_tungstenite::connect_async(r).await.unwrap_err()), 403);
    }

    #[tokio::test]
    async fn phone_address_must_be_owner() {
        let echo = echo_server().await;
        let guest = crate::mobile::MobileUser { name: "손님".into(), slug: "abcdefghijklmnopqrstuvwxy".into(), created: 0, owner: false };
        let owner = crate::mobile::MobileUser { owner: true, ..guest.clone() };
        let base = app(Some(guest)).await;
        let e = tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.unwrap_err();
        assert_eq!(status_of(e), 403);
        let base = app(Some(owner)).await;
        assert!(tokio_tungstenite::connect_async(format!("{base}/net/tcp?port={echo}")).await.is_ok());
    }

    fn target(app: &str, port: u16) -> Target {
        Target { label: "시험".into(), machine: None, base: app.replacen("ws://", "http://", 1), port }
    }

    async fn roundtrip(local: u16, send: &[u8]) -> Vec<u8> {
        let mut s = TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        s.write_all(send).await.unwrap();
        s.shutdown().await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut got)).await.unwrap().unwrap();
        got
    }

    /// 원본 포트가 이 기기에서 이미 쓰이고 있으니(같은 기기 시험) 빈 번호로 물러서고, 쓰기를 닫은 뒤에도 응답을
    /// 끝까지 받고, 닫으면 그 번호가 거부된다.
    #[tokio::test(flavor = "multi_thread")]
    async fn forward_carries_half_close_and_stop_closes() {
        let echo = echo_server().await;
        let base = app(None).await;
        let opened = open(target(&base, echo), None, "cli").await.unwrap();
        assert_ne!(opened.local, echo, "쓰이는 번호를 가로챘다");
        assert_eq!(roundtrip(opened.local, b"GET / HTTP/1.0\r\n\r\n").await, b"GET / HTTP/1.0\r\n\r\n");
        let big: Vec<u8> = (0..300_000u32).map(|i| i as u8).collect();
        assert_eq!(roundtrip(opened.local, &big).await, big);
        if opened.v6 {
            let mut s = TcpStream::connect(("::1", opened.local)).await.unwrap();
            s.write_all(b"v6").await.unwrap();
            s.shutdown().await.unwrap();
            let mut got = Vec::new();
            s.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, b"v6");
        }
        let again = open(target(&base, echo), None, "show").await.unwrap();
        assert!(again.reused && again.local == opened.local, "같은 기기·포트는 다시 쓴다");
        stop(opened.local).await.unwrap();
        assert!(TcpStream::connect(("127.0.0.1", opened.local)).await.is_err());
        assert!(stop(opened.local).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forward_refuses_what_it_cannot_reach() {
        let base = app(None).await;
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = open(target(&base, dead), None, "cli").await.err().unwrap();
        assert!(e.contains("듣는 게 없어요"), "{e}");
        let echo = echo_server().await;
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = busy.local_addr().unwrap().port();
        let e = open(target(&base, echo), Some(taken), "cli").await.err().unwrap();
        assert!(e.contains("이미 누가"), "{e}");
    }

    #[test]
    fn only_loopback_urls_are_pulled_and_host_is_kept() {
        let (u, p) = loopback_url("http://localhost:5173/a?b=1#c").unwrap();
        assert_eq!(p, 5173);
        assert_eq!(rewrite(u, 40000, true), "http://localhost:40000/a?b=1#c");
        let (u, p) = loopback_url("https://127.0.0.1/x").unwrap();
        assert_eq!(p, 443);
        assert_eq!(rewrite(u, 5173, false), "https://127.0.0.1:5173/x");
        let (u, _) = loopback_url("ws://[::1]:8080/hmr").unwrap();
        assert_eq!(rewrite(u.clone(), 8080, true), "ws://[::1]:8080/hmr");
        assert_eq!(rewrite(u, 8080, false), "ws://localhost:8080/hmr");
        let (u, _) = loopback_url("http://127.0.0.2:3000/").unwrap();
        assert_eq!(rewrite(u, 3000, true), "http://localhost:3000/");
        for no in ["http://example.com:3000", "http://192.168.0.2:3000", "file:///tmp/a", "http://u:p@localhost:3000", "http://localhost.evil.com/"] {
            assert!(loopback_url(no).is_none(), "{no}");
        }
    }
}
