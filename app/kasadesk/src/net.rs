//! 보기 앱의 카사넷 노드 — 이 기기 보기 앱 키(`identity::view_key_path`, 본 앱이 `/version` 의 `view_id` 로 알린다)로 뜨고
//! 아무 연결도 받지 않는다. 기기마다 그 기기 화면 서비스 노드(있으면)와 앱 노드로 길을 쥐고, 데이터를 실어도 되는 길(직통·
//! 국내 중계)일 때만 화면을 붙인다 — 본 앱 `kasanet::connect_screen` 과 같은 판정이다.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kasa_cast::{Config, Watch, SCREEN_ALPN, SCREEN_LINK_ALPN};
use kasa_castview::{OnEvent, Start, OLD_SENDER, WAITING_PATH};
use kasa_net::iroh::endpoint::{presets, Connection, PortmapperConfig};
use kasa_net::iroh::{Endpoint, EndpointAddr, EndpointId};
use kasa_net::{identity, peer, AllowList, Link, LinkState, RelayTrust};
use serde_json::Value;

use crate::roster::Machine;

/// 검증 리그용 — UDP 를 이 주소(예: `127.0.0.2:0`)에만 연다. 본 앱과 같은 이름이다.
const BIND_ENV: &str = "KASATERM_KASANET_BIND";

/// 기기 하나와의 길 — 화면 서비스 노드와 앱 노드.
#[derive(Default)]
struct Paths {
    service: Option<Link>,
    app: Option<Link>,
    /// 그 기기 앱이 화면 스트림을 받는 판이다(`kasanet.screen`).
    screen: bool,
}

pub struct Net {
    pub handle: tokio::runtime::Handle,
    endpoint: Endpoint,
    trust: RelayTrust,
    paths: Mutex<HashMap<String, Paths>>,
}

impl Net {
    /// 보기 앱 키로 엔드포인트를 띄운다. 허용 목록이 비어 들어오는 연결은 핸드셰이크에서 다 끊긴다.
    pub fn start(handle: tokio::runtime::Handle) -> anyhow::Result<Arc<Self>> {
        let path = identity::view_key_path().ok_or_else(|| anyhow::anyhow!("홈 폴더를 모른다"))?;
        let key = identity::load_or_create(&path)?;
        let own = kasa_net::relay::own_relays();
        let endpoint = handle.block_on(async {
            let mut builder = kasa_net::builder(presets::Minimal, key, &AllowList::default()).relay_mode(kasa_net::relay::relay_mode(&own));
            let rig = std::env::var(BIND_ENV).ok().and_then(|v| v.trim().parse::<std::net::SocketAddr>().ok());
            if let Some(at) = rig {
                builder = builder.clear_ip_transports().bind_addr(at)?;
                if at.ip().is_loopback() {
                    // 루프백 리그는 공유기에 묻지 않는다 — UPnP 가 0.0.0.0 을 열어 방화벽 묻기 창이 사람 화면에 뜬다.
                    builder = builder.portmapper_config(PortmapperConfig::Disabled);
                }
            }
            anyhow::Ok(builder.bind().await?)
        })?;
        eprintln!("[kasadesk] 보기 앱 {} 준비", endpoint.id().fmt_short());
        let _rt = handle.enter();
        tokio::spawn(kasa_net::relay::fall_back_when_denied(endpoint.clone(), own.clone(), |line| eprintln!("[kasadesk] {line}")));
        tokio::spawn(kasa_net::portmap::keep_swept(endpoint.clone(), |s| {
            eprintln!("[kasadesk] 공유기 UPnP 매핑 정리 — 이 노드 것 {}칸 중 {}칸 지움", s.mine, s.removed);
        }));
        Ok(Arc::new(Self { handle: handle.clone(), endpoint, trust: RelayTrust::new(own), paths: Mutex::new(HashMap::new()) }))
    }

    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// 명부를 따라 길을 세우고 고친다. 명부에서 빠진 기기의 길은 놓는다.
    pub fn follow(&self, machines: &[Machine]) {
        let _rt = self.handle.enter();
        let mut paths = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        paths.retain(|label, _| machines.iter().any(|m| &m.label == label));
        for m in machines {
            let p = paths.entry(m.label.clone()).or_default();
            p.screen = m.kasanet.get("screen").and_then(Value::as_bool).unwrap_or(false);
            let service = m.kasanet.get("screen_node").and_then(peer::addr_from_json).filter(|a| a.id != self.id());
            self.keep(&mut p.service, service, SCREEN_LINK_ALPN, &format!("{} 화면 서비스", m.label));
            let app = peer::addr_from_json(&m.kasanet).filter(|a| a.id != self.id());
            self.keep(&mut p.app, app, kasa_net::fwd::ALPN, &m.label);
        }
    }

    fn keep(&self, slot: &mut Option<Link>, addr: Option<EndpointAddr>, alpn: &'static [u8], name: &str) {
        match (slot.as_ref(), addr) {
            (Some(link), Some(addr)) if link.id() == addr.id => link.set_addr(addr),
            (_, Some(addr)) => {
                eprintln!("[kasadesk] {name} {} 길 시작", addr.id.fmt_short());
                *slot = Some(Link::start_alpn(self.endpoint.clone(), addr, self.trust.clone(), alpn));
            }
            (_, None) => *slot = None,
        }
    }

    /// 그 기기 화면에 붙는 길. 서비스 노드가 데이터를 실을 길이면 서비스로(본 앱이 다시 떠도 이어진다), 아니면 앱 노드로.
    /// 둘 다 아니면 까닭을 들고 기다린다.
    pub fn start_view(self: &Arc<Self>, label: &str) -> Start {
        let paths = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = paths.get(label) else { return Start::Wait("명부에서 그 기기를 아직 못 찾았어요 — 이 기기 카사텀을 띄우면 배워요".into()) };
        let carrying = |l: &Option<Link>| l.as_ref().filter(|l| l.state().carries_data()).map(|l| (l.addr(), path_name(l.state())));
        let target = carrying(&p.service).or_else(|| carrying(&p.app).filter(|_| p.screen));
        let Some((addr, path)) = target else { return Start::Wait(waiting_text(p)) };
        let me = self.clone();
        Start::Connect(Box::new(move |config: Config, on: OnEvent| -> Option<Watch> {
            let endpoint = me.endpoint.clone();
            let connect = async move {
                let conn: Connection = endpoint.connect(addr, SCREEN_ALPN).await.map_err(|e| format!("연결 실패: {e}"))?;
                Ok((conn, path))
            };
            Some(kasa_cast::spawn_watch(&me.handle, connect, config, on))
        }))
    }

    /// 고르기 장면에 표시할 길 — 서비스 노드 길이 서면 그 길, 아니면 앱 노드 길.
    /// (표시할 글, 길이 서 있나) — 서 있으면 고르기 방에서 그 기기 화면이 켜진다.
    pub fn path_status(&self, label: &str) -> (String, bool) {
        let paths = self.paths.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = paths.get(label) else { return ("주소 모름".into(), false) };
        let show = |l: &Link, who: &str| match l.state() {
            LinkState::Direct { rtt } => Some(format!("{who} 직통 {}ms", rtt.as_millis())),
            LinkState::TrustedRelay { rtt } => Some(format!("{who} 국내 중계 {}ms", rtt.as_millis())),
            _ => None,
        };
        p.service
            .as_ref()
            .and_then(|l| show(l, "서비스"))
            .or_else(|| p.app.as_ref().filter(|_| p.screen).and_then(|l| show(l, "앱")))
            .map_or_else(|| (short_wait(p).into(), false), |text| (text, true))
    }
}

fn path_name(state: LinkState) -> &'static str {
    match state {
        LinkState::Direct { .. } => "kasanet",
        _ => "kasanet-relay",
    }
}

/// 그 기기가 이 보기 앱 id 를 모른다 — 허용 목록 훅의 거절(`kasanet: unknown endpoint`)이 길의 마지막 오류에 남는다.
fn refused(p: &Paths) -> bool {
    [&p.service, &p.app].into_iter().flatten().any(|l| l.last_error().is_some_and(|e| e.contains("unknown endpoint") || e.contains("403")))
}

fn waiting_text(p: &Paths) -> String {
    if p.service.is_none() && !p.screen {
        return OLD_SENDER.into();
    }
    if refused(p) {
        return "그 기기 카사텀이 이 보기 앱을 아직 몰라요 — 그 기기와 이 기기 카사텀을 업데이트하면 보여요".into();
    }
    WAITING_PATH.into()
}

fn short_wait(p: &Paths) -> &'static str {
    if p.service.is_none() && !p.screen {
        "화면을 못 보내는 판"
    } else if refused(p) {
        "이 보기 앱을 모름"
    } else {
        "길 찾는 중"
    }
}
