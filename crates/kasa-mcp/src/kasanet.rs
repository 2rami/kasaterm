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

use kasa_net::iroh::endpoint::presets;
use kasa_net::iroh::protocol::Router;
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
    /// 등록된 폰 → 수명이 끝나는 때.
    phones: Mutex<HashMap<EndpointId, Instant>>,
}

/// 폰 등록 수명. 폰은 이 3분의 1마다 관문으로 다시 등록한다.
pub const PHONE_TTL: Duration = Duration::from_secs(15 * 60);
const PHONE_SWEEP: Duration = Duration::from_secs(30);

/// 폰 입구 포트 — HTTP 서버가 리스너를 열면 알린다. 없으면 폰을 받지 않는다(HTTP 포트로 돌릴 수 없다).
static PHONE_INGRESS: OnceLock<u16> = OnceLock::new();

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
    // 올리지 않는다. 데이터는 국내 자체 중계로만 싣고 n0 중계는 구멍 뚫기 신호에만 쓴다.
    let own_relays = kasa_net::relay::own_relays();
    let mut builder = kasa_net::builder(presets::Minimal, key, &allow)
        .relay_mode(kasa_net::relay::relay_mode(&own_relays));
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
        .accept(fwd::ALPN, fwd_server.clone())
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
        phones: Mutex::new(HashMap::new()),
    };
    if NODE.set(node).is_err() {
        return;
    }
    tokio::spawn(sweep_phones());
    if let Some(n) = NODE.get() {
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
    if id == n.endpoint.id() || n.links.lock().is_ok_and(|l| l.contains_key(&id)) {
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

/// 앱 안에서(Safari 화면) 데스크톱 localhost 를 여는 폰 앱이 있다고 보는 기간. 그런 판만 카사넷 등록을 하므로 마지막 등록
/// 시각으로 판을 가른다 — 등록 수명(15분)으로 가르면 폰 앱이 잠든 사이 보여 주기가 임시 터널로 떨어진다.
const PHONE_APP_FRESH: Duration = Duration::from_secs(30 * 24 * 3600);
const PHONE_APP_FILE: &str = "kasanet-phone-app.json";
static PHONE_APP_AT: Mutex<Option<u64>> = Mutex::new(None);

/// 키 옆. 격리 인스턴스가 이번 실행 전용 키로 떴으면(시험도) 파일 없이 메모리에만.
fn phone_app_path() -> Option<std::path::PathBuf> {
    if cfg!(test) || (std::env::var_os(identity::KEY_PATH_ENV).is_none() && isolated()) {
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
pub(crate) fn note_phone_app(id: Option<EndpointId>) {
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
}
