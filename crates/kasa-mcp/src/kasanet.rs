//! 카사넷 배선 — 다른 기기 카사텀으로 가는 HTTP·거울을 직통 QUIC 로 싣는다. 설계는 `docs/kasanet.md`.
//!
//! 기기 주소(`Machine.base`)는 바꾸지 않는다. 대신 base 마다 로컬 입구(`kasa_net::Route`)를 두고,
//! 요청을 보내는 자리가 `route_base(base)` 로 그 입구를 받는다. 입구는 연결마다 직통이면 카사넷,
//! 아니면 원래 base(ssh 터널 등)로 잇는다. 공용 중계로는 데이터를 싣지 않는다(P0: ssh 길보다 느리다).
//!
//! 허용 목록: 루프백 base(앱이 든 ssh 터널·손으로 든 터널·손님의 되돌아오는 -R)로 물은 `/version` 이
//! 알려 준 EndpointId 만 넣는다. 그 길은 ssh 인증을 거쳤다. 카사넷으로 들어온 연결은 받는 쪽에서
//! `127.0.0.1` 로 이어져 ssh -L 과 같은 신뢰를 얻으므로, 이 목록이 ssh 문과 같은 무게의 문이다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use kasa_net::iroh::endpoint::presets;
use kasa_net::iroh::protocol::Router;
use kasa_net::iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use kasa_net::{fwd, identity, AllowList, FwdServer, Link, LinkState, Route};
use serde_json::Value;

/// 끄는 스위치. `0`·`off` 면 엔드포인트를 띄우지 않고 모든 길이 예전 그대로다.
const OFF_ENV: &str = "KASATERM_KASANET";
/// 검증 전용 — 이 밀리초 뒤 엔드포인트를 닫아 「직통이 끊기면 ssh 로 돌아가는가」를 본다.
const STOP_AFTER_ENV: &str = "KASATERM_KASANET_STOP_MS";
/// 검증 리그용 — UDP 를 이 주소(예: `127.0.0.1:0`)에만 연다. 서명 안 된 디버그 앱이 0.0.0.0 을 열면 macOS 방화벽이
/// 사람 화면에 묻기 창을 띄우고, 답하기 전까지 들어오는 UDP 를 막아 직통이 안 선다(2026-09-29 리그에서 중계만 잡힘).
const BIND_ENV: &str = "KASATERM_KASANET_BIND";

struct Node {
    endpoint: Endpoint,
    router: Router,
    allow: AllowList,
    mcp_port: u16,
    handle: tokio::runtime::Handle,
    links: Mutex<HashMap<EndpointId, Link>>,
    routes: Mutex<HashMap<String, Arc<Route>>>,
}

static NODE: OnceLock<Node> = OnceLock::new();

fn disabled() -> bool {
    std::env::var(OFF_ENV).is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false"))
}

/// 격리 인스턴스(검증 앱)가 본판 키로 뜨면 상대가 두 기기를 한 기기로 본다. 키 경로를 따로 안 줬으면
/// 이번 실행만 쓰는 키로 뜬다.
fn isolated() -> bool {
    [
        "KASATERM_SESSION_FILE",
        "KASATERM_SETTINGS_FILE",
        "KASATERM_COLLAB_ROOT",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

fn secret() -> std::io::Result<SecretKey> {
    if std::env::var_os(identity::KEY_PATH_ENV).is_none() && isolated() {
        return Ok(SecretKey::generate());
    }
    let path =
        identity::default_key_path().ok_or_else(|| std::io::Error::other("홈 폴더를 모른다"))?;
    identity::load_or_create(&path)
}

/// 앱 HTTP 서버가 뜰 때 한 번. `mcp_port` 는 이 앱의 HTTP 포트 — 상대가 카사넷으로 닿을 수 있는 곳은
/// 그것과 카사크롬 다리(8777)뿐이다.
pub async fn start(mcp_port: u16) {
    if disabled() || NODE.get().is_some() {
        return;
    }
    let key = match secret() {
        Ok(key) => key,
        Err(e) => {
            eprintln!("[kasanet] 신원 키를 못 읽어 끈다: {e}");
            return;
        }
    };
    let allow = AllowList::default();
    // 주소 찾기(pkarr 공개)는 쓰지 않는다 — 상대 주소는 /version 으로 직접 받고, 기기 IP 를 공용 DNS 에
    // 올리지 않는다. 중계는 구멍 뚫기 신호에만 쓴다.
    let mut builder =
        kasa_net::builder(presets::Minimal, key, &allow).relay_mode(RelayMode::Default);
    if let Some(at) = std::env::var(BIND_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<SocketAddr>().ok())
    {
        builder = match builder.clear_ip_transports().bind_addr(at) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[kasanet] {BIND_ENV}={at} 로 못 묶는다: {e}");
                return;
            }
        };
    }
    let bound = builder.bind().await;
    let endpoint = match bound {
        Ok(ep) => ep,
        Err(e) => {
            eprintln!("[kasanet] 엔드포인트를 못 띄웠다: {e}");
            return;
        }
    };
    let fwd_server = FwdServer::new([mcp_port, crate::machines::KASACHROME_PORT]);
    let router = Router::builder(endpoint.clone())
        .accept(fwd::ALPN, fwd_server)
        .spawn();
    eprintln!(
        "[kasanet] {} 준비 — 받는 포트 {mcp_port}·{}",
        endpoint.id().fmt_short(),
        crate::machines::KASACHROME_PORT
    );
    let node = Node {
        endpoint,
        router,
        allow,
        mcp_port,
        handle: tokio::runtime::Handle::current(),
        links: Mutex::new(HashMap::new()),
        routes: Mutex::new(HashMap::new()),
    };
    if NODE.set(node).is_err() {
        return;
    }
    if let Some(ms) = std::env::var(STOP_AFTER_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if let Some(n) = NODE.get() {
                eprintln!("[kasanet] {STOP_AFTER_ENV} — 엔드포인트를 닫는다");
                let _ = n.router.shutdown().await;
            }
        });
    }
}

/// 앱을 끌 때 — 상대가 유휴 시간 초과를 기다리지 않고 바로 ssh 길로 돌아가게 닫았다고 알린다.
pub fn shutdown() {
    let Some(n) = NODE.get() else { return };
    let (tx, rx) = std::sync::mpsc::channel();
    n.handle.spawn(async move {
        let _ = n.router.shutdown().await;
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(Duration::from_millis(500));
}

/// `/version` 에 싣는 이 기기의 카사넷 주소. 엔드포인트가 없으면 None — 옛 판과 같은 모양이 된다.
pub fn info() -> Option<Value> {
    let n = NODE.get()?;
    let addr = n.endpoint.addr();
    Some(serde_json::json!({
        "id": addr.id.to_string(),
        "relay": addr.relay_urls().next().map(|u| u.to_string()),
        "addrs": addr.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
        "port": n.mcp_port,
    }))
}

fn parse_peer(v: &Value) -> Option<(EndpointAddr, u16)> {
    let id = EndpointId::from_str(v.get("id")?.as_str()?).ok()?;
    let port = u16::try_from(v.get("port")?.as_u64()?)
        .ok()
        .filter(|p| *p != 0)?;
    let mut addr = EndpointAddr::new(id);
    if let Some(relay) = v
        .get("relay")
        .and_then(Value::as_str)
        .and_then(|r| RelayUrl::from_str(r).ok())
    {
        addr = addr.with_relay_url(relay);
    }
    for ip in v
        .get("addrs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(sock) = ip.as_str().and_then(|s| s.parse::<SocketAddr>().ok()) {
            addr = addr.with_ip_addr(sock);
        }
    }
    Some((addr, port))
}

/// base 가 이 기기 루프백을 가리키면 그 소켓 주소. 루프백이 아니면(LAN 주소를 손으로 적은 항목) None —
/// 평문 HTTP 로 받은 id 를 믿으면 같은 망의 누구든 허용 목록에 들어올 수 있다.
fn loopback(base: &str) -> Option<SocketAddr> {
    let rest = base.trim().trim_end_matches('/').strip_prefix("http://")?;
    let hostport = rest.split('/').next()?;
    let (host, port) = hostport.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    match host {
        "127.0.0.1" | "localhost" => Some(SocketAddr::from(([127, 0, 0, 1], port))),
        _ => None,
    }
}

fn key(base: &str) -> String {
    base.trim().trim_end_matches('/').to_string()
}

fn route_for(n: &Node, base: &str) -> Option<Arc<Route>> {
    let fallback = loopback(base)?;
    let mut routes = n.routes.lock().ok()?;
    if let Some(r) = routes.get(&key(base)) {
        return Some(r.clone());
    }
    let _rt = n.handle.enter();
    let route = Arc::new(Route::start(fallback).ok()?);
    routes.insert(key(base), route.clone());
    Some(route)
}

/// 이 base 로 요청을 보낼 때 쓸 주소. 루프백 base 면 입구 주소, 아니면 base 그대로.
/// 입구는 상대를 모르는 동안 원래 base 로 그대로 잇는다 — 앱이 막 떠 거울을 되붙일 때도 입구를 거쳐야
/// 나중에 직통이 섰을 때 그 거울을 옮길 수 있다.
pub fn route_base(base: &str) -> String {
    NODE.get()
        .and_then(|n| route_for(n, base))
        .map(|r| format!("http://{}", r.local_addr()))
        .unwrap_or_else(|| base.to_string())
}

/// 루프백 base 로 물은 `/version` 의 `kasanet` 을 배운다. 상대를 허용 목록에 넣고 그 base 의 입구에 잇는다.
pub fn learn(base: &str, kasanet: &Value) {
    let Some(n) = NODE.get() else { return };
    if loopback(base).is_none() {
        return;
    }
    let Some((addr, port)) = parse_peer(kasanet) else {
        return;
    };
    if addr.id == n.endpoint.id() {
        static WARNED: OnceLock<()> = OnceLock::new();
        if WARNED.set(()).is_ok() {
            eprintln!("[kasanet] {base} 가 이 기기와 같은 키를 쓴다 — 격리 인스턴스라면 KASATERM_KASANET_KEY 를 따로 줘라");
        }
        return;
    }
    let fresh = n.allow.insert(addr.id);
    let link = {
        let Ok(mut links) = n.links.lock() else {
            return;
        };
        match links.get(&addr.id) {
            Some(link) => {
                link.set_addr(addr);
                link.clone()
            }
            None => {
                let _rt = n.handle.enter();
                let link = Link::start(n.endpoint.clone(), addr);
                links.insert(link.id(), link.clone());
                tokio::spawn(log_changes(base.to_string(), link.clone()));
                link
            }
        }
    };
    if fresh {
        eprintln!(
            "[kasanet] {base} = {} 허용·연결 시작",
            link.id().fmt_short()
        );
    }
    if let Some(route) = route_for(n, base) {
        let _rt = n.handle.enter();
        route.set_link(link, port);
    }
}

/// 길이 바뀔 때만 한 줄 — 「왜 ssh 로 가나」를 앱 로그에서 바로 읽게.
async fn log_changes(base: String, link: Link) {
    let mut rx = link.subscribe();
    let mut said_error: Option<String> = None;
    loop {
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { return },
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                let error = link.last_error();
                if error.is_some() && error != said_error && !link.state().is_direct() {
                    eprintln!("[kasanet] {base} ({}) {}", link.id().fmt_short(), error.as_deref().unwrap_or_default());
                    said_error = error;
                }
                continue;
            }
        }
        let state = *rx.borrow_and_update();
        let what = match state {
            LinkState::Direct { rtt } => format!("직통 {}ms", rtt.as_millis()),
            LinkState::Relay => "중계만 — 원래 길로 보낸다".into(),
            LinkState::Down => "끊김 — 원래 길로 보낸다".into(),
        };
        eprintln!("[kasanet] {base} ({}) {what}", link.id().fmt_short());
    }
}

/// 기기 상태에 싣는 길. 입구가 직통이면 ("kasanet", 왕복), 아니면 ("ssh", None). 입구가 없으면 None.
pub fn path(base: &str) -> Option<(&'static str, Option<u64>)> {
    let n = NODE.get()?;
    let route = n.routes.lock().ok()?.get(&key(base))?.clone();
    Some(match route.link_state() {
        Some(LinkState::Direct { rtt }) => ("kasanet", Some(rtt.as_millis() as u64)),
        _ => ("ssh", None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_bases_are_trusted() {
        assert_eq!(
            loopback("http://127.0.0.1:18795"),
            Some(SocketAddr::from(([127, 0, 0, 1], 18795)))
        );
        assert_eq!(
            loopback("http://localhost:8765/"),
            Some(SocketAddr::from(([127, 0, 0, 1], 8765)))
        );
        assert_eq!(loopback("http://10.1.2.3:8765"), None);
        assert_eq!(loopback("https://127.0.0.1:8765"), None);
        assert_eq!(loopback("http://127.0.0.1"), None);
    }

    #[test]
    fn peer_needs_id_and_port() {
        let id = SecretKey::generate().public().to_string();
        let v = serde_json::json!({"id": id, "port": 8765, "addrs": ["1.2.3.4:5", "bad"], "relay": "https://r.example./"});
        let (addr, port) = parse_peer(&v).unwrap();
        assert_eq!(port, 8765);
        assert_eq!(addr.ip_addrs().count(), 1);
        assert_eq!(addr.relay_urls().count(), 1);
        assert!(parse_peer(&serde_json::json!({"id": id})).is_none());
        assert!(parse_peer(&serde_json::json!({"id": "nope", "port": 1})).is_none());
    }
}
