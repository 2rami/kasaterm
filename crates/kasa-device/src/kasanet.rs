//! 카사넷 배선 — 다른 기기 카사텀으로 가는 HTTP·거울을 직통 QUIC 로 싣는다. 설계는 `docs/kasanet.md`.
//!
//! 기기 주소(`Machine.base`)는 바꾸지 않는다. 대신 base 마다 로컬 입구(`kasa_net::Route`)를 두고,
//! 요청을 보내는 자리가 `route_base(base)` 로 그 입구를 받는다. 입구는 연결마다 직통이면 카사넷,
//! 아니면 원래 base(ssh 터널 등)로 잇는다. 중계는 국내 자체 중계(`kasa_net::relay`)로만 싣고, n0 공용 중계로는
//! 싣지 않는다(P0: ssh 길보다 느리다).
//!
//! 허용 목록: 루프백 base(앱이 든 ssh 터널·손으로 든 터널·손님의 되돌아오는 -R)로 물은 `/version` 이
//! 알려 준 EndpointId 만 넣는다. 그 길은 ssh 인증을 거쳤다. 카사넷으로 들어온 연결은 받는 쪽에서
//! `127.0.0.1` 로 이어져 ssh -L 과 같은 신뢰를 얻으므로, 이 목록이 ssh 문과 같은 무게의 문이다.
//!
//! 폰은 다르다(`allow_phone`): 관문을 거쳐 주인 폰 자격으로 온 등록만 받고, 들어온 연결은 HTTP 포트 대신
//! 폰 입구로 돌린다 — 폰 입구는 관문 업링크와 같은 자격(`ViaUplink`, 주인 주소 아래)만 준다. 등록은 수명이
//! 있어 폰이 관문으로 계속 다시 등록해야 산다. 관문에서 폐기된 폰은 다시 등록을 못 해 수명 안에 끊긴다.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kasa_net::iroh::endpoint::{presets, Connection, PortmapperConfig};
use kasa_net::iroh::protocol::{AcceptError, ProtocolHandler, Router};
use kasa_net::iroh::{Endpoint, EndpointId, SecretKey};
use kasa_net::{fwd, identity, peer, AllowList, FwdServer, Link, LinkState, RelayTrust, Route};
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
    fwd: FwdServer,
    trust: RelayTrust,
    mcp_port: u16,
    handle: tokio::runtime::Handle,
    links: Mutex<HashMap<EndpointId, Link>>,
    routes: Mutex<HashMap<String, Arc<Route>>>,
    /// base → 상대 id 와 화면 스트림을 받는 판인지. `connect` 가 base 로 상대를 찾는다.
    bases: Mutex<HashMap<String, (EndpointId, bool)>>,
    /// base → 그 기기 화면 서비스 노드와의 길(`/version` 의 `screen_node`). 앱이 다시 떠도 이 길의 화면은 이어진다.
    screens: Mutex<HashMap<String, Link>>,
    /// base → 그 기기 보기 앱 id(`/version` 의 `view_id`). 화면만 받고 포트는 하나도 못 연다(`docs/screen-service-split.md` 「보기 앱」).
    views: Mutex<HashMap<String, EndpointId>>,
    /// 관문 계정으로 로그인한 보기 앱(`POST /kasanet/view`) → 수명이 끝나는 때. 폰처럼 다시 등록해야 산다.
    account_views: Mutex<HashMap<EndpointId, Instant>>,
    /// 등록된 폰 → 수명이 끝나는 때.
    phones: Mutex<HashMap<EndpointId, Instant>>,
}

/// 폰 등록 수명. 폰은 이 3분의 1마다 관문으로 다시 등록한다.
pub const PHONE_TTL: Duration = Duration::from_secs(15 * 60);
const PHONE_SWEEP: Duration = Duration::from_secs(30);

/// 폰 입구 포트 — HTTP 서버가 리스너를 열면 알린다. 없으면 폰을 받지 않는다(HTTP 포트로 돌릴 수 없다).
static PHONE_INGRESS: OnceLock<u16> = OnceLock::new();

static NODE: OnceLock<Node> = OnceLock::new();

/// 화면 스트림(`docs/screen-stream.md`). 받는 일은 위층(`kasa_remote::screen_stream`)이 꽂는다 — 이 층은 화면을 모른다.
pub use kasa_cast::{SCREEN_ALPN, SCREEN_LINK_ALPN};
pub type ScreenHandler =
    Arc<dyn Fn(Connection) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;
static SCREEN: OnceLock<ScreenHandler> = OnceLock::new();

/// 앱이 부팅 때 한 번. 꽂기 전·꽂지 않은 판(관문·lite)은 화면 연결을 바로 닫고 `/version` 에 알리지 않는다.
pub fn set_screen_handler(handler: ScreenHandler) {
    let _ = SCREEN.set(handler);
}

#[derive(Debug, Clone)]
struct ScreenAccept;

impl ProtocolHandler for ScreenAccept {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        // 허용 목록 훅을 이미 지났다. 그래도 명부 기기와 등록된 주인 폰만 받는다 — 화면에는 메일·비밀번호 창까지
        // 보인다. 지금 `/screen/ws` 에 카사넷으로 닿을 수 있는 범위와 같다.
        let peer = conn.remote_id();
        let known = NODE.get().is_some_and(|n| {
            n.links.lock().is_ok_and(|l| l.contains_key(&peer))
                || n.phones.lock().is_ok_and(|p| p.contains_key(&peer))
                || n.views.lock().is_ok_and(|v| v.values().any(|id| *id == peer))
                || n.account_views.lock().is_ok_and(|v| v.contains_key(&peer))
        });
        match SCREEN.get() {
            Some(handler) if known => handler(conn).await,
            _ => conn.close(403u32.into(), b"kasanet: screen refused"),
        }
        Ok(())
    }
}

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
    // 올리지 않는다. 데이터는 국내 자체 중계로만 싣고 n0 중계는 구멍 뚫기 신호에만 쓴다.
    let own_relays = kasa_net::relay::own_relays();
    let mut builder = kasa_net::builder(presets::Minimal, key, &allow)
        .relay_mode(kasa_net::relay::relay_mode(&own_relays));
    let rig_bind = std::env::var(BIND_ENV).ok().and_then(|v| v.trim().parse::<SocketAddr>().ok());
    // 루프백 리그는 공유기에 묻지 않는다 — UPnP 찾기·매핑·정리가 0.0.0.0 UDP 를 열어 Windows 방화벽이 사람 화면에
    // 「이 앱에 액세스를 허용할까요」를 띄운다(2026-11-16 Windows 리그). 루프백 길에는 공유기 매핑이 쓸모도 없다.
    let loopback_rig = rig_bind.is_some_and(|a| a.ip().is_loopback());
    if let Some(at) = rig_bind {
        builder = match builder.clear_ip_transports().bind_addr(at) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[kasanet] {BIND_ENV}={at} 로 못 묶는다: {e}");
                return;
            }
        };
    }
    if loopback_rig {
        builder = builder.portmapper_config(PortmapperConfig::Disabled);
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
        .accept(fwd::ALPN, fwd_server.clone())
        .accept(SCREEN_ALPN, ScreenAccept)
        .spawn();
    eprintln!(
        "[kasanet] {} 준비 — 받는 포트 {mcp_port}·{}",
        endpoint.id().fmt_short(),
        crate::machines::KASACHROME_PORT
    );
    let relay_watch = endpoint.clone();
    let node = Node {
        endpoint,
        router,
        allow,
        fwd: fwd_server,
        trust: RelayTrust::new(own_relays.clone()),
        mcp_port,
        handle: tokio::runtime::Handle::current(),
        links: Mutex::new(HashMap::new()),
        routes: Mutex::new(HashMap::new()),
        bases: Mutex::new(HashMap::new()),
        screens: Mutex::new(HashMap::new()),
        views: Mutex::new(HashMap::new()),
        account_views: Mutex::new(HashMap::new()),
        phones: Mutex::new(HashMap::new()),
    };
    if NODE.set(node).is_err() {
        return;
    }
    if let Some(n) = NODE.get() {
        for (base, node) in load_screen_nodes() {
            learn_screen_node(n, &base, Some(&node));
        }
    }
    tokio::spawn(sweep_phones());
    if !loopback_rig {
        tokio::spawn(report_ids());
    }
    if let Some(n) = NODE.get().filter(|_| !loopback_rig) {
        tokio::spawn(kasa_net::portmap::keep_swept(n.endpoint.clone(), |s| {
            eprintln!("[kasanet] 공유기 UPnP 매핑 정리 — 이 기기 것 {}칸 중 {}칸 지움", s.mine, s.removed);
        }));
    }
    tokio::spawn(kasa_net::relay::fall_back_when_denied(
        relay_watch,
        own_relays,
        |line| eprintln!("[kasanet] {line}"),
    ));
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

/// 관문 계정에 알릴 이 기기의 카사넷 id — 앱 노드·보기 앱·화면 서비스 노드, 그리고 관문으로 주인을 확인해 받은 폰·보기 앱.
/// 관문은 이 목록을 국내 중계 허용 파일로 내놓는다(`kasa-gateway` `gateway_kasanet.rs`). 관문 상한(기기당 16)을 넘지 않게 자른다.
fn reportable_ids() -> Vec<String> {
    let Some(n) = NODE.get() else { return Vec::new() };
    let mut ids = vec![n.endpoint.id().to_string()];
    ids.extend(local_view_id().map(|id| id.to_string()));
    let screen = SCREEN_NODE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|v| v.get("id")?.as_str().map(str::to_string));
    ids.extend(screen);
    let now = Instant::now();
    for list in [&n.phones, &n.account_views] {
        ids.extend(list.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, until)| **until > now).map(|(id, _)| id.to_string()));
    }
    ids.sort();
    ids.dedup();
    ids.truncate(16);
    ids
}

/// 관문 로그인이 있으면 이 기기 카사넷 id 를 알린다 — 손으로 `relay.sh allow` 하지 않아도 국내 중계를 쓴다. 목록이 바뀌었을 때와
/// 한 시간마다(관문이 다시 떠 잃었을 때). 옛 관문(404)이면 다음 시간까지 묻지 않는다.
async fn report_ids() {
    const EVERY: Duration = Duration::from_secs(60);
    const REFRESH: Duration = Duration::from_secs(3600);
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none()).build() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut last: Option<(Vec<String>, String, Instant)> = None;
    tokio::time::sleep(Duration::from_secs(20)).await;
    loop {
        let ids = reportable_ids();
        if let Some((cred, _)) = crate::device_auth::capture().filter(|_| !ids.is_empty()) {
            let relay = cred.relay.trim_end_matches('/').to_string();
            let due = last.as_ref().is_none_or(|(sent, to, at)| *sent != ids || *to != relay || at.elapsed() >= REFRESH);
            if due {
                let sent = client.post(format!("{relay}/relay/kasanet/ids")).bearer_auth(&cred.token)
                    .json(&serde_json::json!({ "ids": ids })).send().await;
                match sent.map(|r| r.status().as_u16()) {
                    Ok(200..=299) => {
                        eprintln!("[kasanet] 관문에 카사넷 id {}개 알림", ids.len());
                        last = Some((ids, relay, Instant::now()));
                    }
                    Ok(404) => last = Some((ids, relay, Instant::now())),
                    Ok(code) => eprintln!("[kasanet] 관문이 카사넷 id 알림을 {code} 로 거절"),
                    Err(e) => eprintln!("[kasanet] 관문에 카사넷 id 를 못 알림: {e}"),
                }
            }
        }
        tokio::time::sleep(EVERY).await;
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

/// 이 기기 화면 서비스 노드(`docs/screen-service-split.md`) — 앱이 제어 길로 받아 `/version` 에 싣는다. 서비스가 없거나
/// 끊기면 None.
static SCREEN_NODE: Mutex<Option<Value>> = Mutex::new(None);

/// 화면 서비스가 알린 노드 `{id, relay, addrs, proto, version}` — 바뀔 때마다 앱이 부른다.
pub fn set_screen_node(node: Option<Value>) {
    *SCREEN_NODE.lock().unwrap_or_else(|e| e.into_inner()) = node;
}

/// 이 기기 보기 앱의 id — 보기 앱 키(`identity::view_key_path`)를 없으면 만들어 읽는다. 처음 띄우기 전부터 알려야 다른 기기가 미리
/// 허용해 둔다. 격리 인스턴스는 보기 앱 키 자리를 따로 줬을 때만(본판 보기 앱 id 를 알리지 않게).
fn local_view_id() -> Option<EndpointId> {
    static VIEW_ID: OnceLock<Option<EndpointId>> = OnceLock::new();
    *VIEW_ID.get_or_init(|| {
        let own_path = std::env::var_os(identity::VIEW_KEY_PATH_ENV).is_some_and(|p| !p.is_empty());
        if cfg!(any(test, feature = "test-isolation")) || (!own_path && std::env::var_os(identity::KEY_PATH_ENV).is_none() && isolated()) {
            return None;
        }
        match identity::load_or_create(&identity::view_key_path()?) {
            Ok(key) => Some(key.public()),
            Err(e) => {
                eprintln!("[kasanet] 보기 앱 키를 못 읽어 알리지 않는다: {e}");
                None
            }
        }
    })
}

/// `/version` 에 싣는 이 기기의 카사넷 주소. 엔드포인트가 없으면 None — 옛 판과 같은 모양이 된다.
pub fn info() -> Option<Value> {
    let n = NODE.get()?;
    let addr = n.endpoint.addr();
    let mut v = serde_json::json!({
        "id": addr.id.to_string(),
        "relay": addr.relay_urls().next().map(|u| u.to_string()),
        "addrs": addr.ip_addrs().map(|a| a.to_string()).collect::<Vec<_>>(),
        "port": n.mcp_port,
        "screen": SCREEN.get().is_some(),
    });
    if let Some(node) = SCREEN_NODE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        v["screen_node"] = node;
    }
    if let Some(id) = local_view_id() {
        v["view_id"] = id.to_string().into();
    }
    Some(v)
}

/// 화면 서비스가 받을 보는 쪽 — 명부로 배운 기기 id·그 기기 보기 앱 id 와 등록된 주인 폰 id·만료(유닉스 초). 앱의 입구
/// (`ScreenAccept`)와 같은 범위다. 서비스 노드는 화면만 받으니 보기 앱 id 를 기기와 같은 줄에 넣는다.
pub fn screen_viewers() -> (Vec<String>, Vec<(String, u64)>) {
    let Some(n) = NODE.get() else { return (Vec::new(), Vec::new()) };
    let mut machines: Vec<String> = n.links.lock().map(|l| l.keys().map(|id| id.to_string()).collect()).unwrap_or_default();
    machines.extend(n.views.lock().map(|v| v.values().map(|id| id.to_string()).collect::<Vec<_>>()).unwrap_or_default());
    machines.sort();
    machines.dedup();
    let now = Instant::now();
    let unix = unix_now();
    let lease = |m: &HashMap<EndpointId, Instant>| -> Vec<(String, u64)> {
        m.iter().map(|(id, until)| (id.to_string(), unix + until.saturating_duration_since(now).as_secs())).collect()
    };
    // 계정 보기 앱은 폰과 같은 꼴(만료 있는 id)이라 서비스가 수명대로 걷게 폰 줄에 싣는다.
    let mut phones: Vec<(String, u64)> = n.phones.lock().map(|p| lease(&p)).unwrap_or_default();
    phones.extend(n.account_views.lock().map(|v| lease(&v)).unwrap_or_default());
    phones.sort();
    (machines, phones)
}

/// base 가 이 기기 루프백을 가리키면 그 소켓 주소. 루프백이 아니면(LAN 주소를 손으로 적은 항목) None —
/// 평문 HTTP 로 받은 id 를 믿으면 같은 망의 누구든 허용 목록에 들어올 수 있다.
fn loopback(base: &str) -> Option<SocketAddr> {
    let rest = base.trim().trim_end_matches('/').strip_prefix("http://")?;
    // A TCP route cannot preserve a proxy path prefix when it falls back to the original endpoint.
    let (host, port) = rest.rsplit_once(':')?;
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
    let Some((addr, port)) = peer::from_json(kasanet) else {
        return;
    };
    if n.phones.lock().is_ok_and(|p| p.contains_key(&addr.id)) {
        return;
    }
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
                let link = Link::start(n.endpoint.clone(), addr, n.trust.clone());
                links.insert(link.id(), link.clone());
                tokio::spawn(log_changes(base.to_string(), link.clone()));
                link
            }
        }
    };
    if let Ok(mut bases) = n.bases.lock() {
        let screen = kasanet.get("screen").and_then(Value::as_bool).unwrap_or(false);
        bases.insert(key(base), (link.id(), screen));
    }
    learn_screen_node(n, base, kasanet.get("screen_node"));
    learn_view(n, base, kasanet.get("view_id"));
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

/// 그 base 의 화면 서비스 노드를 배운다 — 있으면 그 노드와 길을 따로 쥐고(`SCREEN_LINK_ALPN`), 없어졌으면 놓는다.
/// 믿는 근거는 앱 id 와 같다: 루프백 base 로 물은 `/version` 이 알려 준 id 다.
fn learn_screen_node(n: &Node, base: &str, node: Option<&Value>) {
    let addr = node.and_then(peer::addr_from_json).filter(|a| a.id != n.endpoint.id());
    remember_screen_node(base, addr.as_ref().and(node));
    let Ok(mut screens) = n.screens.lock() else { return };
    match addr {
        Some(addr) => match screens.get(&key(base)) {
            Some(link) if link.id() == addr.id => link.set_addr(addr),
            _ => {
                let _rt = n.handle.enter();
                let link = Link::start_alpn(n.endpoint.clone(), addr, n.trust.clone(), SCREEN_LINK_ALPN);
                eprintln!("[kasanet] {base} 화면 서비스 {} 길 시작", link.id().fmt_short());
                tokio::spawn(log_changes(format!("{base} 화면 서비스"), link.clone()));
                screens.insert(key(base), link);
            }
        },
        None => {
            screens.remove(&key(base));
        }
    }
}

/// 그 base 의 보기 앱 id 를 배운다 — 기기 id 와 같은 근거(루프백 base 로 물은 `/version`)로 믿되 화면만 내준다.
fn learn_view(n: &Node, base: &str, id: Option<&Value>) {
    let id = id
        .and_then(Value::as_str)
        .and_then(|s| EndpointId::from_str(s.trim()).ok())
        .filter(|id| *id != n.endpoint.id())
        .filter(|id| !n.links.lock().is_ok_and(|l| l.contains_key(id)) && !n.phones.lock().is_ok_and(|p| p.contains_key(id)));
    let Ok(mut views) = n.views.lock() else { return };
    if let Some(id) = apply_view(&mut views, &n.allow, &n.fwd, base, id) {
        eprintln!("[kasanet] {base} 보기 앱 {} 허용 — 화면만", id.fmt_short());
    }
}

/// 보기 앱 표 한 칸을 바꾼다. 포트 돌리기(빈 표 — 어느 포트도 못 연다)를 허용 목록보다 먼저 건다 — 먼저 들면 그 틈에 붙은
/// 연결이 HTTP 포트로 샌다. 다른 base 가 같은 id 를 쥐고 있지 않으면 옛 id 는 걷는다. 새로 든 id 를 돌려준다.
fn apply_view(
    views: &mut HashMap<String, EndpointId>,
    allow: &AllowList,
    fwd: &FwdServer,
    base: &str,
    id: Option<EndpointId>,
) -> Option<EndpointId> {
    let before = views.get(&key(base)).copied();
    if before == id {
        return None;
    }
    if let Some(old) = before {
        views.remove(&key(base));
        let leased = NODE.get().is_some_and(|n| n.account_views.lock().is_ok_and(|v| v.contains_key(&old)));
        if !views.values().any(|v| *v == old) && !leased {
            allow.remove(&old);
            fwd.forget(&old);
        }
    }
    let id = id?;
    fwd.redirect(id, []);
    allow.insert(id);
    views.insert(key(base), id);
    Some(id)
}

/// 배운 화면 서비스 노드를 키 옆에 남긴다 — 그 기기 앱이 꺼져 `/version` 이 안 와도(앱을 안 띄운 맥미니·앱 재시작 중) 다음 실행이
/// 그 노드로 붙는다(`docs/screen-service-split.md` 「앱이 없을 때」). 공개키·주소라 비밀이 아니다.
fn screen_nodes_path() -> Option<std::path::PathBuf> {
    if cfg!(any(test, feature = "test-isolation")) || (std::env::var_os(identity::KEY_PATH_ENV).is_none() && isolated()) {
        return None;
    }
    Some(identity::default_key_path()?.with_file_name("kasanet-screens.json"))
}

static SCREEN_NODES: Mutex<Option<serde_json::Map<String, Value>>> = Mutex::new(None);

fn load_screen_nodes() -> Vec<(String, Value)> {
    let map = screen_nodes_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let out = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    *SCREEN_NODES.lock().unwrap_or_else(|e| e.into_inner()) = Some(map);
    out
}

fn remember_screen_node(base: &str, node: Option<&Value>) {
    let Some(path) = screen_nodes_path() else { return };
    let mut guard = SCREEN_NODES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(serde_json::Map::new);
    let before = map.get(&key(base)).cloned();
    match node {
        Some(v) => {
            map.insert(key(base), v.clone());
        }
        None => {
            map.remove(&key(base));
        }
    }
    if map.get(&key(base)) != before.as_ref() {
        let _ = std::fs::write(path, Value::Object(map.clone()).to_string());
    }
}

/// HTTP 서버가 폰 입구 리스너를 연 뒤 한 번.
pub fn set_phone_ingress(port: u16) {
    let _ = PHONE_INGRESS.set(port);
}

/// 관문을 거쳐 주인 폰 자격으로 온 등록(`POST /kasanet/phone`)만 부른다. 수명을 돌려준다.
/// 폰 연결은 이 앱 HTTP 포트로 와도 폰 입구로 가고, 다른 포트(카사크롬 다리 등)는 못 연다.
pub fn allow_phone(id: &str) -> Result<Duration, &'static str> {
    let n = NODE.get().ok_or("kasanet_off")?;
    let ingress = *PHONE_INGRESS.get().ok_or("phone_ingress_off")?;
    let id = EndpointId::from_str(id.trim()).map_err(|_| "bad_id")?;
    // 기기로 배운 id 를 폰으로 덮으면 그 기기의 거울 길이 폰 입구로 돌아가 버린다.
    if id == n.endpoint.id()
        || n.links.lock().is_ok_and(|l| l.contains_key(&id))
        || n.views.lock().is_ok_and(|v| v.values().any(|view| *view == id))
    {
        return Err("id_taken");
    }
    // 돌리기를 먼저 건다 — 허용 목록에 먼저 들면 그 틈에 붙은 연결이 HTTP 포트로 샌다.
    n.fwd.redirect(id, [(n.mcp_port, ingress)]);
    n.allow.insert(id);
    let fresh = n
        .phones
        .lock()
        .map_err(|_| "busy")?
        .insert(id, Instant::now() + PHONE_TTL)
        .is_none();
    if fresh {
        eprintln!("[kasanet] 폰 {} 허용", id.fmt_short());
        note_phone_app(Some(id));
    }
    Ok(PHONE_TTL)
}

/// 관문 계정으로 로그인한 보기 앱의 등록(`POST /kasanet/view`, 관문을 거친 주인 자격). 기기 명부로 배운 보기 앱처럼
/// 화면만 내주되(빈 포트 돌리기) 폰처럼 수명이 있다 — 관문에서 그 기기를 폐기하면 다시 등록을 못 해 수명 안에 빠진다.
/// 폰 등록 길을 빌리지 않는다 — 폰 앱이 있다는 표식(`note_phone_app`)이 켜져 「폰 보여 주기」 길이 바뀐다.
pub fn allow_viewer(id: &str) -> Result<Duration, &'static str> {
    let n = NODE.get().ok_or("kasanet_off")?;
    let id = EndpointId::from_str(id.trim()).map_err(|_| "bad_id")?;
    if id == n.endpoint.id()
        || n.links.lock().is_ok_and(|l| l.contains_key(&id))
        || n.phones.lock().is_ok_and(|p| p.contains_key(&id))
    {
        return Err("id_taken");
    }
    n.fwd.redirect(id, []);
    n.allow.insert(id);
    let fresh = n
        .account_views
        .lock()
        .map_err(|_| "busy")?
        .insert(id, Instant::now() + PHONE_TTL)
        .is_none();
    if fresh {
        eprintln!("[kasanet] 계정 보기 앱 {} 허용 — 화면만", id.fmt_short());
    }
    Ok(PHONE_TTL)
}

/// 앱 안에서(Safari 화면) 데스크톱 localhost 를 여는 폰 앱이 있다고 보는 기간. 그런 판만 카사넷 등록을 하므로 마지막 등록
/// 시각으로 판을 가른다 — 등록 수명(15분)으로 가르면 폰 앱이 잠든 사이 보여 주기가 임시 터널로 떨어진다.
const PHONE_APP_FRESH: Duration = Duration::from_secs(30 * 24 * 3600);
const PHONE_APP_FILE: &str = "kasanet-phone-app.json";
static PHONE_APP_AT: Mutex<Option<u64>> = Mutex::new(None);

/// 키 옆. 격리 인스턴스가 이번 실행 전용 키로 떴으면(시험도) 파일 없이 메모리에만.
fn phone_app_path() -> Option<std::path::PathBuf> {
    if cfg!(any(test, feature = "test-isolation")) || (std::env::var_os(identity::KEY_PATH_ENV).is_none() && isolated()) {
        return None;
    }
    Some(identity::default_key_path()?.with_file_name(PHONE_APP_FILE))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `phones` 에 폰 id 를 남긴다 — 국내 중계 허용 목록에 넣을 폰을 관문이 주인 폰으로 확인해 준 id 에서 고르게
/// (`tools/kasanet-relay/relay.sh allow`). 공개키라 비밀이 아니다.
pub fn note_phone_app(id: Option<EndpointId>) {
    let now = unix_now();
    if let Ok(mut at) = PHONE_APP_AT.lock() {
        *at = Some(now);
    }
    if let Some(path) = phone_app_path() {
        let mut phones = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("phones").cloned())
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        if let (Some(id), Some(map)) = (id, phones.as_object_mut()) {
            map.insert(id.to_string(), now.into());
        }
        let _ = std::fs::write(
            path,
            serde_json::json!({ "registered_at": now, "phones": phones }).to_string(),
        );
    }
}

/// 폰 보여 주기에서 이 기기 localhost 주소를 임시 터널 없이 넘겨도 되나 — 새 판 폰 앱이 최근에 등록한 적이 있다.
/// 그 앱은 쪽지의 localhost 주소를 앱 안 Safari 화면으로 연다(직통이면 카사넷, 아니면 관문 `/net/tcp`).
pub fn phone_app_opens_localhost() -> bool {
    let remembered = PHONE_APP_AT.lock().ok().and_then(|at| *at);
    let at = remembered.or_else(|| {
        let text = std::fs::read_to_string(phone_app_path()?).ok()?;
        serde_json::from_str::<Value>(&text)
            .ok()?
            .get("registered_at")?
            .as_u64()
    });
    at.is_some_and(|at| unix_now().saturating_sub(at) < PHONE_APP_FRESH.as_secs())
}

async fn sweep_phones() {
    loop {
        tokio::time::sleep(PHONE_SWEEP).await;
        let Some(n) = NODE.get() else { return };
        let now = Instant::now();
        let expired: Vec<EndpointId> = match n.phones.lock() {
            Ok(mut p) => {
                let gone: Vec<_> = p
                    .iter()
                    .filter(|(_, t)| **t <= now)
                    .map(|(id, _)| *id)
                    .collect();
                for id in &gone {
                    p.remove(id);
                }
                gone
            }
            Err(_) => continue,
        };
        for id in expired {
            n.allow.remove(&id);
            n.fwd.forget(&id);
            eprintln!("[kasanet] 폰 {} 등록 만료 — 끊는다", id.fmt_short());
        }
        let lapsed: Vec<EndpointId> = match n.account_views.lock() {
            Ok(mut v) => {
                let gone: Vec<_> = v.iter().filter(|(_, t)| **t <= now).map(|(id, _)| *id).collect();
                for id in &gone {
                    v.remove(id);
                }
                gone
            }
            Err(_) => continue,
        };
        for id in lapsed {
            // 같은 보기 앱이 명부로도 배운 것이면(그 기기 본 앱이 알린 view_id) 그 길로 계속 받는다.
            if n.views.lock().is_ok_and(|v| v.values().any(|x| *x == id)) {
                continue;
            }
            n.allow.remove(&id);
            n.fwd.forget(&id);
            eprintln!("[kasanet] 계정 보기 앱 {} 등록 만료 — 끊는다", id.fmt_short());
        }
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
                if error.is_some() && error != said_error && !link.state().carries_data() {
                    eprintln!("[kasanet] {base} ({}) {}", link.id().fmt_short(), error.as_deref().unwrap_or_default());
                    said_error = error;
                }
                continue;
            }
        }
        let state = *rx.borrow_and_update();
        let what = match state {
            LinkState::Direct { rtt } => format!("직통 {}ms", rtt.as_millis()),
            LinkState::TrustedRelay { rtt } => format!("자체 중계 {}ms", rtt.as_millis()),
            LinkState::Relay => "공용 중계만 — 원래 길로 보낸다".into(),
            LinkState::Down => "끊김 — 원래 길로 보낸다".into(),
        };
        eprintln!("[kasanet] {base} ({}) {what}", link.id().fmt_short());
    }
}

/// 그 기기가 화면 스트림을 받는 판인가(`/version` 의 `kasanet.screen`, 또는 화면 서비스 노드).
pub fn screen_capable(base: &str) -> bool {
    NODE.get().is_some_and(|n| {
        n.bases.lock().ok().and_then(|b| b.get(&key(base)).map(|(_, screen)| *screen)).unwrap_or(false)
            || n.screens.lock().is_ok_and(|s| s.contains_key(&key(base)))
    })
}

/// 화면을 실을 길 — 화면 서비스 노드와 직통·믿는 중계면 그 길, 아니면 앱 노드의 길(`path`).
pub fn screen_path(base: &str) -> Option<(&'static str, Option<u64>)> {
    let n = NODE.get()?;
    let service = n.screens.lock().ok().and_then(|s| s.get(&key(base)).map(Link::state));
    match service {
        Some(LinkState::Direct { rtt }) => Some(("kasanet", Some(rtt.as_millis() as u64))),
        Some(LinkState::TrustedRelay { rtt }) => Some(("kasanet-relay", Some(rtt.as_millis() as u64))),
        _ => path(base),
    }
}

/// 그 기기 화면에 `kasa/screen/1` 연결을 연다 — 화면 서비스 노드와 데이터를 실을 길이면 서비스로(앱이 다시 떠도 이어진다),
/// 아니면 앱 노드로(옛 판·서비스 없는 기기).
pub async fn connect_screen(base: &str) -> Result<(Connection, &'static str), String> {
    let n = NODE.get().ok_or("카사넷이 꺼져 있다")?;
    let service = n.screens.lock().ok().and_then(|s| s.get(&key(base)).cloned());
    if let Some(link) = service {
        let path = match link.state() {
            LinkState::Direct { .. } => Some("kasanet"),
            LinkState::TrustedRelay { .. } => Some("kasanet-relay"),
            _ => None,
        };
        if let Some(path) = path {
            match n.endpoint.connect(link.addr(), SCREEN_ALPN).await {
                Ok(conn) => return Ok((conn, path)),
                Err(e) => eprintln!("[kasanet] {base} 화면 서비스에 못 붙어 앱으로: {e}"),
            }
        }
    }
    connect(base, SCREEN_ALPN).await
}

/// 카사넷 일꾼 런타임 — 위층이 같은 런타임에서 연결을 돌린다.
pub fn runtime() -> Option<tokio::runtime::Handle> {
    NODE.get().map(|n| n.handle.clone())
}

/// base 의 기기에 `alpn` 연결을 따로 연다. 데이터를 실어도 되는 길(직통·믿는 중계)일 때만 — 아니면 그 까닭을
/// 돌려준다. 돌려주는 글은 `path` 와 같은 길 이름이다.
pub async fn connect(base: &str, alpn: &[u8]) -> Result<(Connection, &'static str), String> {
    let n = NODE.get().ok_or("카사넷이 꺼져 있다")?;
    let id = n
        .bases
        .lock()
        .ok()
        .and_then(|b| b.get(&key(base)).map(|(id, _)| *id))
        .ok_or("그 기기의 카사넷 주소를 아직 모른다")?;
    let link = n
        .links
        .lock()
        .ok()
        .and_then(|l| l.get(&id).cloned())
        .ok_or("그 기기와 이은 길이 없다")?;
    let path = match link.state() {
        LinkState::Direct { .. } => "kasanet",
        LinkState::TrustedRelay { .. } => "kasanet-relay",
        _ => return Err("직통도 믿는 중계도 아니다".into()),
    };
    let conn = n
        .endpoint
        .connect(link.addr(), alpn)
        .await
        .map_err(|e| format!("연결 실패: {e}"))?;
    Ok((conn, path))
}

/// 기기 상태에 싣는 길. 직통이면 ("kasanet", 왕복), 믿는 중계면 ("kasanet-relay", 왕복), 아니면 ("ssh", None).
/// 입구가 없으면 None.
pub fn path(base: &str) -> Option<(&'static str, Option<u64>)> {
    let n = NODE.get()?;
    let route = n.routes.lock().ok()?.get(&key(base))?.clone();
    Some(match route.link_state() {
        Some(LinkState::Direct { rtt }) => ("kasanet", Some(rtt.as_millis() as u64)),
        Some(LinkState::TrustedRelay { rtt }) => ("kasanet-relay", Some(rtt.as_millis() as u64)),
        _ => ("ssh", None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_app_id_is_allowed_for_screens_only_and_swapped_cleanly() {
        let (allow, fwd) = (AllowList::default(), FwdServer::new([8765, 8777]));
        let mut views = HashMap::new();
        let (a, b) = (SecretKey::generate().public(), SecretKey::generate().public());
        assert_eq!(apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18795", Some(a)), Some(a));
        assert!(allow.contains(&a));
        assert_eq!(apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18795/", Some(a)), None, "같은 id 는 그대로");
        assert_eq!(apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18795", Some(b)), Some(b));
        assert!(!allow.contains(&a) && allow.contains(&b), "키를 갈면 옛 id 는 걷는다");
        assert_eq!(apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18796", Some(b)), Some(b));
        apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18795", None);
        assert!(allow.contains(&b), "다른 기기 칸이 같은 id 를 쥐고 있으면 남긴다");
        apply_view(&mut views, &allow, &fwd, "http://127.0.0.1:18796", None);
        assert!(!allow.contains(&b) && views.is_empty());
    }

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
        assert_eq!(loopback("http://127.0.0.1:8765/m/peer"), None);
        assert_eq!(loopback("http://localhost:8765/prefix/?token=fixture"), None);
    }
}
