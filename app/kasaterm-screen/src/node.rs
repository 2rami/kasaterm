//! 서비스의 카사넷 화면 노드 — 서비스 자기 키로 뜨고 `kasa/screen/1` 만 받는다(`docs/screen-service-split.md` 「카사넷」).
//! 앱 노드와 같은 중계 지도·국내 중계 허용 목록 밖 물러나기·UPnP 치우기를 쓰고, `fwd` 는 받지 않는다.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kasa_cast::audio::Hub;
use kasa_cast::{Capture, SCREEN_ALPN, SCREEN_LINK_ALPN};
use kasa_net::iroh::endpoint::{presets, Connection, PortmapperConfig};
use kasa_net::iroh::protocol::{AcceptError, ProtocolHandler, Router};
use kasa_net::iroh::{Endpoint, EndpointId, SecretKey};
use kasa_net::AllowList;
use serde_json::{json, Value};

use crate::state::Settings;

/// 검증 리그용 — UDP 를 이 주소에만 연다(앱 노드와 같은 이름). 루프백이면 공유기에 묻지 않는다.
const BIND_ENV: &str = "KASATERM_KASANET_BIND";

pub struct Node {
    pub endpoint: Endpoint,
    router: Router,
    allow: AllowList,
    allowed: Mutex<HashSet<EndpointId>>,
    pub viewers: Arc<Viewers>,
}

/// 지금 붙어 보는 쪽 — 앱의 「보는 중」 표시와 업데이트 도우미의 「보는 사람이 떠날 때까지」가 읽는다.
#[derive(Default)]
pub struct Viewers {
    count: AtomicUsize,
    ids: Mutex<Vec<EndpointId>>,
}

impl Viewers {
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    pub fn list(&self) -> Vec<String> {
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|id| id.fmt_short().to_string()).collect()
    }

    fn enter(&self, id: EndpointId) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.ids.lock().unwrap_or_else(|e| e.into_inner()).push(id);
    }

    fn leave(&self, id: EndpointId) {
        self.count.fetch_sub(1, Ordering::Relaxed);
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = ids.iter().position(|x| *x == id) {
            ids.remove(i);
        }
    }
}

#[derive(Clone)]
struct Screen {
    capture: Arc<dyn Capture>,
    hub: Option<Arc<Hub>>,
    settings: Arc<Mutex<Settings>>,
    viewers: Arc<Viewers>,
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Screen")
    }
}

impl ProtocolHandler for Screen {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // 허용 목록 훅(핸드셰이크 직후)을 이미 지났다 — 앱이 넘긴 명부 기기·주인 폰이다.
        let peer = conn.remote_id();
        self.viewers.enter(peer);
        eprintln!("[screen-service] {} 보기 시작", peer.fmt_short());
        let sound = if self.settings.lock().unwrap_or_else(|e| e.into_inner()).audio { self.hub.clone() } else { None };
        kasa_cast::serve_conn_audio(conn, Some(self.capture.clone()), sound).await;
        self.viewers.leave(peer);
        eprintln!("[screen-service] {} 보기 끝", peer.fmt_short());
        Ok(())
    }
}

/// 보는 쪽 앱이 이 노드와의 길을 재려고 쥐는 연결 — 스트림 없이 닫힐 때까지 둔다.
#[derive(Debug, Clone)]
struct LinkHold;

impl ProtocolHandler for LinkHold {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        conn.closed().await;
        Ok(())
    }
}

impl Node {
    pub async fn start(
        key: SecretKey,
        capture: Arc<dyn Capture>,
        hub: Option<Arc<Hub>>,
        settings: Arc<Mutex<Settings>>,
        allowed: &[EndpointId],
    ) -> anyhow::Result<Self> {
        let allow = AllowList::new(allowed.iter().copied());
        let own_relays = kasa_net::relay::own_relays();
        let mut builder = kasa_net::builder(presets::Minimal, key, &allow).relay_mode(kasa_net::relay::relay_mode(&own_relays));
        let rig_bind = std::env::var(BIND_ENV).ok().and_then(|v| v.trim().parse::<SocketAddr>().ok());
        let loopback_rig = rig_bind.is_some_and(|a| a.ip().is_loopback());
        if let Some(at) = rig_bind {
            builder = builder.clear_ip_transports().bind_addr(at)?;
        }
        if loopback_rig {
            builder = builder.portmapper_config(PortmapperConfig::Disabled);
        }
        let endpoint = builder.bind().await?;
        let viewers = Arc::new(Viewers::default());
        let router = Router::builder(endpoint.clone())
            .accept(SCREEN_ALPN, Screen { capture, hub, settings, viewers: viewers.clone() })
            .accept(SCREEN_LINK_ALPN, LinkHold)
            .spawn();
        if !loopback_rig {
            tokio::spawn(kasa_net::portmap::keep_swept(endpoint.clone(), |s| {
                eprintln!("[screen-service] 공유기 UPnP 매핑 정리 — 이 노드 것 {}칸 중 {}칸 지움", s.mine, s.removed);
            }));
        }
        tokio::spawn(kasa_net::relay::fall_back_when_denied(endpoint.clone(), own_relays, |line| eprintln!("[screen-service] {line}")));
        eprintln!("[screen-service] 화면 노드 {} 준비 — 받는 보는 쪽 {}", endpoint.id().fmt_short(), allowed.len());
        Ok(Self { endpoint, router, allow, allowed: Mutex::new(allowed.iter().copied().collect()), viewers })
    }

    /// 앱이 넘긴 목록으로 통째 바꾼다. 빠진 id 는 다음 연결부터 막는다(붙어 있는 보기는 그 앱이 닫을 때까지 둔다).
    pub fn set_allowed(&self, ids: &[EndpointId]) {
        let next: HashSet<EndpointId> = ids.iter().copied().collect();
        let mut cur = self.allowed.lock().unwrap_or_else(|e| e.into_inner());
        for gone in cur.difference(&next) {
            self.allow.remove(gone);
        }
        for new in next.difference(&cur) {
            self.allow.insert(*new);
        }
        *cur = next;
    }

    /// `/version` 의 `screen_node` 로 실릴 주소 — 앱이 제어 길로 받아 싣는다.
    pub fn info(&self, version: &str, proto: u32) -> Value {
        let addr = self.endpoint.addr();
        json!({
            "id": addr.id.to_string(),
            "relay": addr.relay_urls().next().map(|u| u.to_string()),
            "addrs": addr.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
            "proto": proto,
            "version": version,
        })
    }

    /// 보는 쪽에 닫았다고 알리고 엔드포인트를 닫는다 — 기다리지 않고 다른 길(앱)로 돌아가게.
    pub async fn shutdown(&self) {
        let _ = self.router.shutdown().await;
    }
}

/// 서비스 키 — 상태 폴더에 둔다(Windows 는 SYSTEM·Administrators 만 읽는다). 앱의 기기 키와 따로다: 한 id 에 엔드포인트가
/// 둘이면 중계와 보는 쪽이 어느 쪽에 닿을지 모른다.
pub fn load_key(path: &std::path::Path) -> std::io::Result<SecretKey> {
    if std::env::var_os("KASATERM_SCREEN_EPHEMERAL_KEY").is_some() {
        return Ok(SecretKey::generate());
    }
    kasa_net::identity::load_or_create(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn only_listed_viewers_get_in_and_the_list_can_change() {
        std::env::set_var(BIND_ENV, "127.0.0.1:0");
        let capture: Arc<dyn Capture> = Arc::new(kasa_cast_host_fake());
        let stranger = SecretKey::generate();
        let friend = SecretKey::generate();
        let node = Node::start(SecretKey::generate(), capture, None, Arc::default(), &[friend.public()]).await.unwrap();
        let viewer = |k: SecretKey| async move {
            Endpoint::builder(presets::Minimal)
                .secret_key(k)
                .relay_mode(kasa_net::iroh::RelayMode::Disabled)
                .portmapper_config(PortmapperConfig::Disabled)
                .clear_ip_transports()
                .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
                .unwrap()
                .bind()
                .await
                .unwrap()
        };
        let addr = node.endpoint.addr();
        let ok = viewer(friend.clone()).await.connect(addr.clone(), SCREEN_LINK_ALPN).await;
        assert!(ok.is_ok(), "명부 기기는 붙는다");
        let no = viewer(stranger.clone()).await;
        let refused = match no.connect(addr.clone(), SCREEN_LINK_ALPN).await {
            Ok(c) => tokio::time::timeout(std::time::Duration::from_secs(5), c.closed()).await.is_ok(),
            Err(_) => true,
        };
        assert!(refused, "모르는 기기는 막는다");
        node.set_allowed(&[stranger.public()]);
        assert!(viewer(stranger).await.connect(addr, SCREEN_LINK_ALPN).await.is_ok(), "목록을 바꾸면 바로 받는다");
        assert_eq!(node.info("1.0.0", 1)["id"], node.endpoint.id().to_string());
        node.shutdown().await;
    }

    fn kasa_cast_host_fake() -> impl Capture {
        struct Never;
        impl Capture for Never {
            fn start(&self, _: &kasa_cast::Config, _: Box<dyn Fn(kasa_cast::Frame) + Send + Sync>) -> Result<Box<dyn kasa_cast::Session>, kasa_cast::Denied> {
                Err(kasa_cast::Denied::Unavailable)
            }
            fn now(&self) -> f64 {
                0.0
            }
        }
        Never
    }
}
