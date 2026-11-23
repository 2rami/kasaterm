//! TCP 포워드, ALPN `kasa/fwd/1`.
//!
//! QUIC 스트림 하나가 TCP 연결 하나다. 여는 쪽이 포트 2바이트(BE)를 쓰고 받는 쪽이 상태 1바이트로
//! 답한 뒤로는 날 바이트를 양방향으로 흘린다. 받는 쪽은 `127.0.0.1` 의 허용 포트로만 잇는다.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const ALPN: &[u8] = b"kasa/fwd/1";
/// 카사텀 HTTP·카사크롬 다리. 나머지는 사용자가 명시로 공유한 포트뿐이다.
pub const DEFAULT_PORTS: [u16; 2] = [8765, 8777];

const OK: u8 = 0;
const PORT_NOT_ALLOWED: u8 = 1;
const CONNECT_FAILED: u8 = 2;

#[derive(Clone, Debug)]
pub struct FwdServer {
    ports: Arc<RwLock<BTreeSet<u16>>>,
    /// 상대별 포트 돌리기. 여기 든 상대는 적힌 포트만 열 수 있고 적힌 로컬 포트로 이어진다 — 기본 허용 포트는
    /// 안 본다. 폰이 데스크톱 HTTP 포트로 와도 관문 경유와 같은 자격의 입구로 가게 하는 자리다.
    redirects: Arc<RwLock<HashMap<EndpointId, HashMap<u16, u16>>>>,
    /// 돌리기에 든 상대의 살아 있는 연결 — 잊을 때 바로 끊는다.
    held: Arc<std::sync::Mutex<HashMap<EndpointId, Vec<Connection>>>>,
}

impl FwdServer {
    pub fn new(ports: impl IntoIterator<Item = u16>) -> Self {
        Self {
            ports: Arc::new(RwLock::new(ports.into_iter().collect())),
            redirects: Arc::default(),
            held: Arc::default(),
        }
    }

    /// 이 상대는 `map` 의 포트만, 짝지은 로컬 포트로 잇는다. 다시 부르면 표를 갈아 끼운다.
    pub fn redirect(&self, peer: EndpointId, map: impl IntoIterator<Item = (u16, u16)>) {
        self.redirects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer, map.into_iter().collect());
    }

    /// 돌리기를 거두고 그 상대의 연결을 끊는다. 끊지 않으면 이미 붙은 연결이 계속 스트림을 연다.
    pub fn forget(&self, peer: &EndpointId) -> bool {
        let had = self
            .redirects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(peer)
            .is_some();
        let conns = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(peer)
            .unwrap_or_default();
        for conn in conns {
            conn.close(0u32.into(), b"kasanet: forgotten");
        }
        had
    }

    fn is_redirected(&self, peer: &EndpointId) -> bool {
        self.redirects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(peer)
    }

    /// `restricted` 는 연결을 받을 때 돌리기에 들어 있었나. 그 뒤 잊혔어도 기본 허용 포트로 새지 않는다.
    fn target(&self, peer: &EndpointId, restricted: bool, port: u16) -> Option<u16> {
        if let Some(map) = self
            .redirects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(peer)
        {
            return map.get(&port).copied();
        }
        (!restricted && self.is_allowed(port)).then_some(port)
    }

    pub fn allow_port(&self, port: u16) -> bool {
        self.ports
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(port)
    }

    pub fn deny_port(&self, port: u16) -> bool {
        self.ports
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&port)
    }

    pub fn is_allowed(&self, port: u16) -> bool {
        self.ports
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&port)
    }

    async fn serve_stream(
        &self,
        peer: EndpointId,
        restricted: bool,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> io::Result<()> {
        let mut hdr = [0u8; 2];
        AsyncReadExt::read_exact(&mut recv, &mut hdr).await?;
        let Some(port) = self.target(&peer, restricted, u16::from_be_bytes(hdr)) else {
            return refuse(send, PORT_NOT_ALLOWED).await;
        };
        let tcp = match TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await {
            Ok(tcp) => tcp,
            Err(_) => return refuse(send, CONNECT_FAILED).await,
        };
        AsyncWriteExt::write_all(&mut send, &[OK]).await?;
        pipe(tcp, send, recv).await
    }
}

impl ProtocolHandler for FwdServer {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id();
        let restricted = self.is_redirected(&peer);
        if restricted {
            self.held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(peer)
                .or_default()
                .push(conn.clone());
        }
        while let Ok((send, recv)) = conn.accept_bi().await {
            let this = self.clone();
            tokio::spawn(async move {
                let _ = this.serve_stream(peer, restricted, send, recv).await;
            });
        }
        if restricted {
            if let Some(v) = self
                .held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&peer)
            {
                v.retain(|c| c.close_reason().is_none());
            }
        }
        Ok(())
    }
}

async fn refuse(mut send: SendStream, status: u8) -> io::Result<()> {
    AsyncWriteExt::write_all(&mut send, &[status]).await?;
    send.finish().map_err(io::Error::other)
}

/// 상대 기기의 `127.0.0.1:port` 로 가는 스트림을 연다. 상대가 허락해야 돌려준다.
pub async fn open(conn: &Connection, port: u16) -> io::Result<(SendStream, RecvStream)> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(io::Error::other)?;
    AsyncWriteExt::write_all(&mut send, &port.to_be_bytes()).await?;
    let mut status = [0u8; 1];
    AsyncReadExt::read_exact(&mut recv, &mut status).await?;
    match status[0] {
        OK => Ok((send, recv)),
        PORT_NOT_ALLOWED => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("kasanet: 상대가 포트 {port} 를 열어 두지 않았다"),
        )),
        CONNECT_FAILED => Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("kasanet: 상대의 127.0.0.1:{port} 에 아무도 없다"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("kasanet: 모르는 상태 {other}"),
        )),
    }
}

/// 최근 [`BULK_WINDOW`] 안에 [`BULK_BYTES`] 넘게 보낸 스트림은 큰 짐으로 보고 같은 연결의 다른 스트림 뒤로 미룬다.
const BULK_WINDOW: Duration = Duration::from_secs(1);
const BULK_BYTES: usize = 32 * 1024;
const BULK_PRIORITY: i32 = -1;

/// 한 상대로 가는 TCP 연결들은 QUIC 연결 하나의 스트림이라 혼잡 창을 나눠 쓴다. 거울이 다시 붙어 칸마다 수백 KB 의
/// 첫 화면이 몰리면 그 뒤에 짧은 HTTP 응답·키 메아리가 줄을 서, 느린 길에선 몇 초씩 늦었다(2026-11-18 맥북 길 —
/// 3시간에 열 번 넘게 길이 흔들려 28칸이 다시 붙었다). 많이 보내는 동안만 낮추고 조용해지면 되돌린다.
#[derive(Debug)]
struct BulkMeter {
    since: Instant,
    bytes: usize,
}

impl BulkMeter {
    fn new(now: Instant) -> Self {
        Self { since: now, bytes: 0 }
    }

    /// `n` 바이트를 더 보낸다 — 지금 큰 짐인가.
    fn feed(&mut self, n: usize, now: Instant) -> bool {
        if now.duration_since(self.since) >= BULK_WINDOW {
            *self = Self::new(now);
        }
        self.bytes += n;
        self.bytes > BULK_BYTES
    }
}

async fn copy_prioritized(src: &mut (impl AsyncRead + Unpin), send: &mut SendStream) -> io::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    let mut meter = BulkMeter::new(Instant::now());
    let mut bulk = false;
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        let heavy = meter.feed(n, Instant::now());
        if heavy != bulk {
            bulk = heavy;
            let _ = send.set_priority(if heavy { BULK_PRIORITY } else { 0 });
        }
        AsyncWriteExt::write_all(send, &buf[..n]).await?;
    }
}

/// 양방향 복사. 한쪽이 EOF 면 반대편에도 EOF 를 전해 반쯤 닫힌 연결(HTTP/1.0 식)이 살아남게 한다.
pub async fn pipe(tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();
    let up = async {
        copy_prioritized(&mut tcp_rx, &mut send).await?;
        send.finish().map_err(io::Error::other)
    };
    let down = async {
        tokio::io::copy(&mut recv, &mut tcp_tx).await?;
        tcp_tx.shutdown().await
    };
    tokio::try_join!(up, down)?;
    Ok(())
}

/// `127.0.0.1:<local_port>` 에서 받은 TCP 연결을 상대 기기의 `remote_port` 로 잇는다.
/// 보내는 쪽도 localhost 로 여는 까닭은 개발 서버 bind·secure context·OAuth 되돌아오기 주소가 그대로 돌게 하려는 것.
pub struct Forward {
    local_addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Forward {
    pub async fn start(
        endpoint: Endpoint,
        remote: EndpointAddr,
        remote_port: u16,
        local_port: u16,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, local_port)).await?;
        let local_addr = listener.local_addr()?;
        let task = tokio::spawn(accept_loop(endpoint, remote, remote_port, listener));
        Ok(Self { local_addr, task })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept_loop(
    endpoint: Endpoint,
    remote: EndpointAddr,
    remote_port: u16,
    listener: TcpListener,
) {
    // 연결은 하나를 같이 쓴다. TCP 연결마다 QUIC 핸드셰이크를 새로 하면 첫 바이트가 한 왕복 늦는다.
    let shared: Arc<Mutex<Option<Connection>>> = Arc::default();
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            continue;
        };
        let (endpoint, remote, shared) = (endpoint.clone(), remote.clone(), shared.clone());
        tokio::spawn(async move {
            let Ok(conn) = connection(&endpoint, &remote, &shared).await else {
                return;
            };
            let Ok((send, recv)) = open(&conn, remote_port).await else {
                return;
            };
            let _ = pipe(tcp, send, recv).await;
        });
    }
}

async fn connection(
    endpoint: &Endpoint,
    remote: &EndpointAddr,
    shared: &Mutex<Option<Connection>>,
) -> io::Result<Connection> {
    let mut slot = shared.lock().await;
    if let Some(conn) = slot.as_ref().filter(|c| c.close_reason().is_none()) {
        return Ok(conn.clone());
    }
    let conn = endpoint
        .connect(remote.clone(), ALPN)
        .await
        .map_err(io::Error::other)?;
    *slot = Some(conn.clone());
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_redirect_opens_no_port() {
        let server = FwdServer::new(DEFAULT_PORTS);
        let viewer = iroh::SecretKey::generate().public();
        let machine = iroh::SecretKey::generate().public();
        server.redirect(viewer, []);
        assert!(DEFAULT_PORTS.iter().all(|p| server.target(&viewer, true, *p).is_none()), "보기 전용 id 는 어느 포트도 못 연다");
        assert_eq!(server.target(&machine, false, 8765), Some(8765), "다른 기기는 그대로");
    }

    #[test]
    fn a_stream_is_bulk_only_while_it_keeps_sending_a_lot() {
        let start = Instant::now();
        let mut meter = BulkMeter::new(start);
        assert!(!meter.feed(16 * 1024, start), "키·짧은 응답은 그대로");
        assert!(!meter.feed(16 * 1024, start + Duration::from_millis(100)));
        assert!(meter.feed(1, start + Duration::from_millis(200)), "1초에 32KB 를 넘으면 큰 짐");
        assert!(!meter.feed(100, start + Duration::from_millis(1300)), "조용해지면 되돌린다");
        assert!(meter.feed(64 * 1024, start + Duration::from_millis(1400)), "다시 몰리면 다시 낮춘다");
    }
}
