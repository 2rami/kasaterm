//! 루프백 두 노드. 중계·주소 찾기 없이 127.0.0.1 로만 잇는다.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kasa_net::iroh::endpoint::{presets, ConnectionError, PortmapperConfig};
use kasa_net::iroh::protocol::Router;
use kasa_net::iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use kasa_net::{fwd, AllowList, Forward, FwdServer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(20);

async fn node(key: SecretKey, allow: &AllowList) -> Endpoint {
    kasa_net::builder(presets::Minimal, key, allow)
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr_of(ep: &Endpoint) -> EndpointAddr {
    let sock = ep
        .bound_sockets()
        .into_iter()
        .find(SocketAddr::is_ipv4)
        .expect("v4 소켓");
    EndpointAddr::new(ep.id()).with_ip_addr(sock)
}

/// 받은 바이트를 그대로 돌려주고, 들어온 연결 수를 센다.
async fn echo_server() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    (port, hits)
}

struct Pair {
    server: Router,
    client: Endpoint,
    server_addr: EndpointAddr,
    fwd: FwdServer,
}

async fn pair(admit_client: bool, ports: impl IntoIterator<Item = u16>) -> Pair {
    let client_key = SecretKey::generate();
    let allow = AllowList::default();
    if admit_client {
        allow.insert(client_key.public());
    }
    let fwd = FwdServer::new(ports);
    let server_ep = node(SecretKey::generate(), &allow).await;
    let server_addr = addr_of(&server_ep);
    let server = Router::builder(server_ep)
        .accept(fwd::ALPN, fwd.clone())
        .spawn();
    let client = node(client_key, &AllowList::default()).await;
    Pair {
        server,
        client,
        server_addr,
        fwd,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn forward_round_trip_with_half_close() {
    let (echo_port, hits) = echo_server().await;
    let p = pair(true, [echo_port]).await;
    let f = Forward::start(p.client.clone(), p.server_addr.clone(), echo_port, 0)
        .await
        .unwrap();
    assert!(f.local_addr().ip().is_loopback());

    // 두 연결이 한 QUIC 연결 위에서 섞이지 않는지, 큰 덩어리와 반쯤 닫기가 끝까지 가는지.
    let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect();
    let run = |data: Vec<u8>| {
        let at = f.local_addr();
        async move {
            let mut s = TcpStream::connect(at).await.unwrap();
            let (mut r, mut w) = s.split();
            let write = async {
                w.write_all(&data).await.unwrap();
                w.shutdown().await.unwrap();
            };
            let mut back = Vec::new();
            let read = r.read_to_end(&mut back);
            let (_, n) = tokio::join!(write, read);
            n.unwrap();
            assert_eq!(back, data);
        }
    };
    timeout(T, async {
        tokio::join!(run(payload.clone()), run(b"hello kasanet".to_vec()))
    })
    .await
    .expect("포워드 왕복이 시간 안에 끝나야 한다");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    p.server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_endpoint_is_cut_before_any_stream() {
    let (echo_port, hits) = echo_server().await;
    let p = pair(false, [echo_port]).await;
    let outcome = timeout(T, async {
        let conn = p.client.connect(p.server_addr.clone(), fwd::ALPN).await?;
        let (_send, _recv) = fwd::open(&conn, echo_port).await?;
        anyhow::Ok(conn)
    })
    .await
    .expect("거절은 빨리 와야 한다");
    let err = outcome.expect_err("모르는 EndpointId 는 스트림을 못 열어야 한다");
    let closed_403 = err.chain().any(|e| {
        matches!(
            e.downcast_ref::<ConnectionError>(),
            Some(ConnectionError::ApplicationClosed(c)) if c.error_code.into_inner() == 403
        )
    }) || format!("{err:#}").contains("403");
    assert!(closed_403, "허용 목록 거절 코드로 끊겨야 한다: {err:#}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "뒤쪽 TCP 까지 닿으면 안 된다"
    );
    p.server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn only_allowed_ports_reach_localhost() {
    let (open_port, open_hits) = echo_server().await;
    let (closed_port, closed_hits) = echo_server().await;
    let p = pair(true, [open_port]).await;
    let conn = timeout(T, p.client.connect(p.server_addr.clone(), fwd::ALPN))
        .await
        .unwrap()
        .unwrap();

    let err = timeout(T, fwd::open(&conn, closed_port))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(closed_hits.load(Ordering::SeqCst), 0);

    // 허용을 거두면 다음 스트림부터 막힌다. 연결 자체는 그대로 쓴다.
    timeout(T, fwd::open(&conn, open_port))
        .await
        .unwrap()
        .unwrap();
    p.fwd.deny_port(open_port);
    let err = timeout(T, fwd::open(&conn, open_port))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(open_hits.load(Ordering::SeqCst), 1);

    // 허용됐어도 아무도 안 듣는 포트는 연결 거부로 돌아온다.
    let idle = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let idle_port = idle.local_addr().unwrap().port();
    drop(idle);
    p.fwd.allow_port(idle_port);
    let err = timeout(T, fwd::open(&conn, idle_port))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
    p.server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn redirected_peer_reaches_only_its_mapped_ports() {
    let (plain_port, plain_hits) = echo_server().await;
    let (ingress_port, ingress_hits) = echo_server().await;
    let p = pair(true, [plain_port]).await;
    let client_id = p.client.id();
    // 폰: 데스크톱 HTTP 포트(여기선 plain_port)로 와도 입구(ingress_port)로 간다.
    p.fwd.redirect(client_id, [(plain_port, ingress_port)]);
    let conn = timeout(T, p.client.connect(p.server_addr.clone(), fwd::ALPN))
        .await
        .unwrap()
        .unwrap();
    let (mut send, mut recv) = timeout(T, fwd::open(&conn, plain_port))
        .await
        .unwrap()
        .unwrap();
    send.write_all(b"ping").await.unwrap();
    send.finish().unwrap();
    let mut back = Vec::new();
    timeout(T, AsyncReadExt::read_to_end(&mut recv, &mut back))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back, b"ping");
    assert_eq!(ingress_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        plain_hits.load(Ordering::SeqCst),
        0,
        "돌린 상대는 원래 포트에 닿지 않는다"
    );

    // 기본 허용 포트라도 표에 없으면 막힌다.
    let err = timeout(T, fwd::open(&conn, ingress_port))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

    // 잊으면 붙어 있던 연결이 끊긴다 — 기본 허용 포트로 새지 않는다.
    assert!(p.fwd.forget(&client_id));
    timeout(T, conn.closed())
        .await
        .expect("잊은 상대의 연결은 끊겨야 한다");
    assert_eq!(plain_hits.load(Ordering::SeqCst), 0);
    p.server.shutdown().await.unwrap();
}
