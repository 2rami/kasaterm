//! 로컬 TCP 입구 하나. 연결마다 그 순간의 길을 고른다 — 직통(또는 믿는 중계)이면 카사넷, 아니면 원래 길(ssh 터널 등).
//!
//! 기기 주소(base)를 바꿔 끼우지 않는 까닭: 거울은 붙을 때의 base 를 평생 들고 재접속하고, 세션 복원과
//! 여러 판정이 그 base 로 기기를 찾는다. 길이 바뀔 때마다 base 가 바뀌면 거울이 기기를 잃는다.
//! 그래서 입구는 그대로 두고 그 뒤에서 길을 바꾸며, 길이 바뀌면 이미 흐르던 연결을 끊어 새 길로 다시
//! 붙게 한다(거울은 스스로 재접속한다).

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle};

use crate::fwd;
use crate::link::{Link, LinkState};

/// 직통이 이만큼 버텨야 원래 길의 연결을 옮긴다 — 잠깐 섰다 무너지는 길로 거울을 흔들지 않게.
#[cfg(not(test))]
const MIGRATE_AFTER: Duration = Duration::from_secs(2);
#[cfg(test)]
const MIGRATE_AFTER: Duration = Duration::from_millis(100);
/// 옮기는 것은 이보다 오래 산 연결(거울 ws·롱폴·재사용 대기 중인 keep-alive)뿐이다. 막 시작한 짧은 요청을
/// 끊으면 보낸 쪽은 실패로 알고 받은 쪽은 처리했을 수 있다.
#[cfg(not(test))]
const LONG_LIVED: Duration = Duration::from_secs(3);
#[cfg(test)]
const LONG_LIVED: Duration = Duration::from_millis(200);
/// 직통인 동안 원래 길에 남은 오래된 연결을 이 간격으로 쓸어 옮긴다. 직통이 설 때 한 번만 옮기면 그 순간
/// 막 열렸던 연결(HTTP 재사용 풀)이 끝까지 ssh 에 남는다(2026-09-29 리그에서 5개 중 3개가 남았다).
const SWEEP: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Kasanet,
    Fallback,
}

pub struct Route {
    inner: Arc<Inner>,
    accept: JoinHandle<()>,
}

struct Inner {
    local_addr: SocketAddr,
    fallback: SocketAddr,
    link: Mutex<Option<(Link, u16)>>,
    watcher: Mutex<Option<JoinHandle<()>>>,
    carried: Mutex<HashMap<u64, Carried>>,
    next: AtomicU64,
}

struct Carried {
    via: Via,
    since: Instant,
    /// 직통인데 카사넷 스트림이 거절돼 원래 길로 온 연결. 옮겨 봐야 또 거절되니 쓸지 않는다.
    pinned: bool,
    abort: AbortHandle,
}

impl Route {
    /// `127.0.0.1` 의 빈 포트에 입구를 연다. tokio 런타임 안에서 부른다.
    pub fn start(fallback: SocketAddr) -> io::Result<Self> {
        let std_listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        std_listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(std_listener)?;
        let inner = Arc::new(Inner {
            local_addr: listener.local_addr()?,
            fallback,
            link: Mutex::new(None),
            watcher: Mutex::new(None),
            carried: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        });
        let accept = tokio::spawn(accept_loop(inner.clone(), listener));
        Ok(Self { inner, accept })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.inner.local_addr
    }

    /// 상대를 알게 됐으면 붙인다. 같은 상대·포트면 아무 일도 안 한다. tokio 런타임 안에서 부른다.
    pub fn set_link(&self, link: Link, remote_port: u16) {
        let mut slot = self.inner.link.lock().unwrap_or_else(|e| e.into_inner());
        if slot
            .as_ref()
            .is_some_and(|(l, p)| l.id() == link.id() && *p == remote_port)
        {
            return;
        }
        let rx = link.subscribe();
        *slot = Some((link, remote_port));
        drop(slot);
        let mut watcher = self.inner.watcher.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = watcher.take() {
            old.abort();
        }
        *watcher = Some(tokio::spawn(follow(self.inner.clone(), rx)));
    }

    pub fn link_state(&self) -> Option<LinkState> {
        self.inner
            .link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(l, _)| l.state())
    }

    /// 지금 들어오는 새 연결이 갈 길.
    pub fn via(&self) -> Via {
        if self.inner.usable().is_some() {
            Via::Kasanet
        } else {
            Via::Fallback
        }
    }

    /// 지금 흐르는 연결 수 (카사넷, 원래 길).
    pub fn carried(&self) -> (usize, usize) {
        let c = self.inner.carried.lock().unwrap_or_else(|e| e.into_inner());
        let k = c.values().filter(|c| c.via == Via::Kasanet).count();
        (k, c.len() - k)
    }
}

impl Drop for Route {
    fn drop(&mut self) {
        self.accept.abort();
        if let Some(w) = self
            .inner
            .watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            w.abort();
        }
        self.inner.cut(|_| true);
    }
}

impl Inner {
    fn usable(&self) -> Option<(Connection, u16)> {
        let slot = self.link.lock().unwrap_or_else(|e| e.into_inner());
        let (link, port) = slot.as_ref()?;
        Some((link.usable()?, *port))
    }

    fn cut(&self, pick: impl Fn(&Carried) -> bool) -> usize {
        let mut c = self.carried.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<u64> = c.iter().filter(|(_, v)| pick(v)).map(|(k, _)| *k).collect();
        for id in &ids {
            if let Some(v) = c.remove(id) {
                v.abort.abort();
            }
        }
        ids.len()
    }

    fn pin_to_fallback(&self, id: u64) {
        if let Some(c) = self
            .carried
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&id)
        {
            c.via = Via::Fallback;
            c.pinned = true;
        }
    }
}

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    loop {
        let Ok((tcp, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let direct = inner.usable();
        let via = if direct.is_some() {
            Via::Kasanet
        } else {
            Via::Fallback
        };
        let id = inner.next.fetch_add(1, Ordering::Relaxed);
        // 등록을 마친 뒤에야 끝난 연결이 자기 줄을 지울 수 있게 잠근 채로 띄운다.
        let mut carried = inner.carried.lock().unwrap_or_else(|e| e.into_inner());
        let task = tokio::spawn({
            let inner = inner.clone();
            async move {
                let _ = carry(&inner, id, tcp, direct).await;
                inner
                    .carried
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
            }
        });
        carried.insert(
            id,
            Carried {
                via,
                since: Instant::now(),
                pinned: false,
                abort: task.abort_handle(),
            },
        );
    }
}

async fn carry(
    inner: &Inner,
    id: u64,
    mut tcp: TcpStream,
    direct: Option<(Connection, u16)>,
) -> io::Result<()> {
    if let Some((conn, port)) = direct {
        match fwd::open(&conn, port).await {
            Ok((send, recv)) => return fwd::pipe(tcp, send, recv).await,
            // 직통이 방금 무너졌거나 상대가 그 포트를 안 받으면 원래 길로.
            Err(_) => inner.pin_to_fallback(id),
        }
    }
    let mut up = TcpStream::connect(inner.fallback).await?;
    tcp.set_nodelay(true)?;
    up.set_nodelay(true)?;
    tokio::io::copy_bidirectional(&mut tcp, &mut up).await?;
    Ok(())
}

async fn follow(inner: Arc<Inner>, mut rx: watch::Receiver<LinkState>) {
    let mut usable_since: Option<Instant> = None;
    loop {
        let now_usable = rx.borrow_and_update().carries_data();
        match (now_usable, usable_since) {
            (true, None) => usable_since = Some(Instant::now()),
            (false, Some(_)) => {
                usable_since = None;
                // 믿지 않는 중계로 떨어진 연결에 데이터를 더 싣지 않는다 — 끊어서 원래 길로 다시 붙게 한다.
                inner.cut(|c| c.via == Via::Kasanet);
            }
            _ => {}
        }
        if usable_since.is_some_and(|t| t.elapsed() >= MIGRATE_AFTER) {
            inner.cut(|c| c.via == Via::Fallback && !c.pinned && c.since.elapsed() >= LONG_LIVED);
        }
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { return },
            _ = tokio::time::sleep(SWEEP), if usable_since.is_some() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fwd, AllowList, FwdServer};
    use iroh::endpoint::{presets, PortmapperConfig};
    use iroh::protocol::Router;
    use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const T: Duration = Duration::from_secs(20);

    async fn node(key: SecretKey, allow: &AllowList) -> Endpoint {
        crate::builder(presets::Minimal, key, allow)
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .clear_ip_transports()
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap()
    }

    /// 한 줄 받을 때마다 `<tag>:<줄>` 로 답한다 — 어느 길로 왔는지 가른다.
    async fn tagged_echo(tag: &'static str) -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let at = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 256];
                    while let Ok(n) = s.read(&mut buf).await {
                        let mut out = format!("{tag}:").into_bytes();
                        out.extend_from_slice(&buf[..n]);
                        if n == 0 || s.write_all(&out).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        at
    }

    /// 답은 스트림이라 여러 번에 나뉘어 올 수 있다 — `<tag>:<msg>` 한 벌이 다 올 때까지 모은다.
    async fn ask(s: &mut TcpStream, msg: &str) -> io::Result<String> {
        s.write_all(msg.as_bytes()).await?;
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        while got.len() < msg.len() + 2 {
            let n = tokio::time::timeout(T, s.read(&mut buf)).await??;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            got.extend_from_slice(&buf[..n]);
        }
        Ok(String::from_utf8_lossy(&got).into_owned())
    }

    async fn wait_until(what: &str, f: impl Fn() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < T, "{what} 이(가) 시간 안에 안 됐다");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn switches_to_direct_migrates_and_falls_back_when_direct_dies() {
        let remote = tagged_echo("K").await;
        let fallback = tagged_echo("F").await;

        let client_key = SecretKey::generate();
        let server_ep = node(
            SecretKey::generate(),
            &AllowList::new([client_key.public()]),
        )
        .await;
        let v4 = server_ep
            .bound_sockets()
            .into_iter()
            .find(SocketAddr::is_ipv4)
            .unwrap();
        let server_addr = EndpointAddr::new(server_ep.id()).with_ip_addr(v4);
        let server = Router::builder(server_ep)
            .accept(fwd::ALPN, FwdServer::new([remote.port()]))
            .spawn();
        let client = node(client_key, &AllowList::default()).await;

        let route = Route::start(fallback).unwrap();
        let at = route.local_addr();

        // 상대를 모르는 동안은 원래 길 그대로.
        let mut old = TcpStream::connect(at).await.unwrap();
        assert_eq!(ask(&mut old, "a").await.unwrap(), "F:a");
        tokio::time::sleep(LONG_LIVED).await;

        let link = Link::start(client.clone(), server_addr, Default::default());
        route.set_link(link.clone(), remote.port());
        wait_until("직통", || link.state().is_direct()).await;
        assert_eq!(route.via(), Via::Kasanet);

        // 오래 산 원래 길 연결은 끊겨 옮겨 가고, 새 연결은 카사넷으로 간다.
        wait_until("옮기기", || route.carried().1 == 0).await;
        assert!(
            ask(&mut old, "b").await.is_err(),
            "옮겨질 연결은 끊겨야 한다"
        );
        let mut fresh = TcpStream::connect(at).await.unwrap();
        assert_eq!(ask(&mut fresh, "c").await.unwrap(), "K:c");
        assert_eq!(route.carried(), (1, 0));

        // 직통이 죽으면 카사넷 연결을 끊고 새 연결은 원래 길로.
        server.shutdown().await.unwrap();
        wait_until("직통 잃음", || !link.state().is_direct()).await;
        wait_until("카사넷 연결 끊음", || route.carried().0 == 0).await;
        assert!(
            ask(&mut fresh, "d").await.is_err(),
            "중계로 떨어진 길에 데이터를 싣지 않는다"
        );
        let mut back = TcpStream::connect(at).await.unwrap();
        assert_eq!(ask(&mut back, "e").await.unwrap(), "F:e");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn peer_that_does_not_know_us_stays_on_fallback() {
        let remote = tagged_echo("K").await;
        let fallback = tagged_echo("F").await;
        let server_ep = node(SecretKey::generate(), &AllowList::default()).await;
        let v4 = server_ep
            .bound_sockets()
            .into_iter()
            .find(SocketAddr::is_ipv4)
            .unwrap();
        let server_addr = EndpointAddr::new(server_ep.id()).with_ip_addr(v4);
        let _server = Router::builder(server_ep)
            .accept(fwd::ALPN, FwdServer::new([remote.port()]))
            .spawn();
        let client = node(SecretKey::generate(), &AllowList::default()).await;

        let route = Route::start(fallback).unwrap();
        route.set_link(
            Link::start(client, server_addr, Default::default()),
            remote.port(),
        );
        // 거절은 핸드셰이크 뒤에 온다 — 그 사이에 직통으로 비쳐 연결을 싣는 일이 없어야 한다.
        for _ in 0..40 {
            assert!(!route.link_state().unwrap().is_direct());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut s = TcpStream::connect(route.local_addr()).await.unwrap();
        assert_eq!(ask(&mut s, "x").await.unwrap(), "F:x");
    }
}
