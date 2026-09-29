//! TCP 포워드, ALPN `kasa/fwd/1`.
//!
//! QUIC 스트림 하나가 TCP 연결 하나다. 여는 쪽이 포트 2바이트(BE)를 쓰고 받는 쪽이 상태 1바이트로
//! 답한 뒤로는 날 바이트를 양방향으로 흘린다. 받는 쪽은 `127.0.0.1` 의 허용 포트로만 잇는다.

use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
}

impl FwdServer {
    pub fn new(ports: impl IntoIterator<Item = u16>) -> Self {
        Self {
            ports: Arc::new(RwLock::new(ports.into_iter().collect())),
        }
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

    async fn serve_stream(&self, mut send: SendStream, mut recv: RecvStream) -> io::Result<()> {
        let mut hdr = [0u8; 2];
        AsyncReadExt::read_exact(&mut recv, &mut hdr).await?;
        let port = u16::from_be_bytes(hdr);
        if !self.is_allowed(port) {
            return refuse(send, PORT_NOT_ALLOWED).await;
        }
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
        while let Ok((send, recv)) = conn.accept_bi().await {
            let this = self.clone();
            tokio::spawn(async move {
                let _ = this.serve_stream(send, recv).await;
            });
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

/// 양방향 복사. 한쪽이 EOF 면 반대편에도 EOF 를 전해 반쯤 닫힌 연결(HTTP/1.0 식)이 살아남게 한다.
pub async fn pipe(tcp: TcpStream, mut send: SendStream, mut recv: RecvStream) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    let (mut tcp_rx, mut tcp_tx) = tcp.into_split();
    let up = async {
        tokio::io::copy(&mut tcp_rx, &mut send).await?;
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
