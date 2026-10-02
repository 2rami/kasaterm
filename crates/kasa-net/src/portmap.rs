//! iroh 포트매퍼가 공유기에 남긴 UPnP 매핑 치우기.
//!
//! 포트매퍼(portmapper)는 2시간 임대로 매핑을 청하지만 ipTIME 같은 공유기는 수명을 무시하고 영구(0)로 둔다. 그런데
//! 매퍼는 앱이 꺼질 때 지우지 않고, 갱신(1시간마다) 때 같은 바깥 포트를 다시 못 얻으면 새 포트로 하나 더 만든다.
//! 그래서 앱이 뜰 때마다·한 시간마다 하나씩 쌓였다 — 실측 공유기 표 48칸 중 40칸이 `iroh-portmap` 이었다(2026-10-02).
//! 표가 차면 새 매핑이 실패해 밖에서 오는 직통 길이 사라진다.
//!
//! 지우는 것은 이 기기 사설 IP·이름 `iroh-portmap`·UDP 인 것뿐이다. 지금 쓰는 받는 포트는 광고 중인 바깥 포트 하나만
//! 남기고, 이 기기에서 아무도 쥐지 않은(묶어 보면 묶이는) 받는 포트의 것은 지운다. 다른 프로세스가 쥔 포트는 그쪽 몫이라 둔다.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use igd_next::aio::tokio::search_gateway;
use igd_next::{PortMappingProtocol, SearchOptions};
use iroh::Endpoint;

const DESCRIPTION: &str = "iroh-portmap";
/// 공유기 표를 끝까지 읽는 상한 — 끝을 알리는 오류를 안 주는 공유기에서 끝없이 돌지 않게.
const MAX_ENTRIES: u32 = 512;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);
/// 처음은 매퍼가 매핑을 세운 뒤, 그다음은 갱신(1시간)이 하나 더 만들어도 오래 남지 않게.
const FIRST_SWEEP: Duration = Duration::from_secs(90);
const SWEEP_EVERY: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub udp: bool,
    pub external_port: u16,
    pub internal_client: String,
    pub internal_port: u16,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    /// 이 기기의 `iroh-portmap` 칸 수(지우기 전).
    pub mine: usize,
    pub removed: usize,
}

/// 지울 바깥 포트들. `entries` 는 공유기 표 순서(먼저 생긴 것 먼저) 그대로다.
/// `live_port` 는 이 엔드포인트가 지금 받는 포트, `advertised` 는 그 포트에 지금 광고 중인 바깥 포트들.
pub fn stale(
    entries: &[Mapping],
    me: Ipv4Addr,
    live_port: Option<u16>,
    advertised: &[u16],
    in_use: impl Fn(u16) -> bool,
) -> Vec<u16> {
    let mine: Vec<&Mapping> = entries
        .iter()
        .filter(|m| {
            m.udp && m.description == DESCRIPTION && m.internal_client.parse::<Ipv4Addr>().ok() == Some(me)
        })
        .collect();
    let mut doomed = Vec::new();
    if let Some(live) = live_port {
        let ours: Vec<&&Mapping> = mine.iter().filter(|m| m.internal_port == live).collect();
        // 광고 중인 것을 남긴다. 모르면 가장 나중 것 — 갱신이 새로 만든 쪽이 매퍼가 지금 쥔 것이다.
        let keep = ours
            .iter()
            .rev()
            .find(|m| advertised.contains(&m.external_port))
            .or(ours.last())
            .map(|m| m.external_port);
        doomed.extend(ours.iter().map(|m| m.external_port).filter(|p| Some(*p) != keep));
    }
    let mut held: HashMap<u16, bool> = HashMap::new();
    for m in mine.iter().filter(|m| Some(m.internal_port) != live_port) {
        if !*held.entry(m.internal_port).or_insert_with(|| in_use(m.internal_port)) {
            doomed.push(m.external_port);
        }
    }
    doomed
}

/// 이 기기에서 누가 이 UDP 포트를 쥐고 있나 — 묶어 보고 안 묶이면 쥔 것이다.
pub fn port_in_use(port: u16) -> bool {
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).is_err()
}

/// 공유기로 나가는 이 기기 주소 — 소켓을 이어 보기만 하고 아무것도 보내지 않는다.
fn local_ip_toward(gateway: SocketAddr) -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect(gateway).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

/// 공유기에 묻고 지운다. UPnP 공유기가 없으면 `Ok(None)`.
pub async fn sweep(live_port: Option<u16>, advertised: &[SocketAddr]) -> anyhow::Result<Option<Swept>> {
    let options = SearchOptions { timeout: Some(SEARCH_TIMEOUT), ..Default::default() };
    // igd_next 는 제 시간 제한을 안 지킬 때가 있다(portmapper 도 같은 이유로 바깥에서 한 번 더 감싼다).
    let Ok(Ok(gateway)) = tokio::time::timeout(SEARCH_TIMEOUT, search_gateway(options)).await else {
        return Ok(None);
    };
    let me = local_ip_toward(gateway.addr).ok_or_else(|| anyhow::anyhow!("공유기로 나가는 주소를 모른다"))?;
    let external = gateway.get_external_ip().await.ok();
    let advertised: Vec<u16> = advertised
        .iter()
        .filter(|a| Some(a.ip()) == external)
        .map(|a| a.port())
        .collect();
    let mut entries = Vec::new();
    for index in 0..MAX_ENTRIES {
        // 표 끝은 오류(SpecifiedArrayIndexInvalid)로 온다. 도중의 오류도 거기서 멈춘다 — 본 것만으로 판정한다.
        let Ok(e) = gateway.get_generic_port_mapping_entry(index).await else { break };
        entries.push(Mapping {
            udp: e.protocol == PortMappingProtocol::UDP,
            external_port: e.external_port,
            internal_client: e.internal_client,
            internal_port: e.internal_port,
            description: e.port_mapping_description,
        });
    }
    let mine = entries
        .iter()
        .filter(|m| m.udp && m.description == DESCRIPTION && m.internal_client.parse::<Ipv4Addr>().ok() == Some(me))
        .count();
    let mut removed = 0;
    for port in stale(&entries, me, live_port, &advertised, port_in_use) {
        if gateway.remove_port(PortMappingProtocol::UDP, port).await.is_ok() {
            removed += 1;
        }
    }
    Ok(Some(Swept { mine, removed }))
}

/// 엔드포인트가 사는 동안 처음 한 번, 그 뒤 30분마다 치운다. 지운 것이 있을 때만 `report` 를 부른다.
pub async fn keep_swept(endpoint: Endpoint, report: impl Fn(Swept)) {
    tokio::time::sleep(FIRST_SWEEP).await;
    while !endpoint.is_closed() {
        let live = endpoint.bound_sockets().into_iter().find(|a| a.is_ipv4()).map(|a| a.port());
        let advertised: Vec<SocketAddr> = endpoint.addr().ip_addrs().copied().collect();
        if let Ok(Some(swept)) = sweep(live, &advertised).await {
            if swept.removed > 0 {
                report(swept);
            }
        }
        tokio::time::sleep(SWEEP_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(external: u16, client: &str, internal: u16, description: &str) -> Mapping {
        Mapping {
            udp: true,
            external_port: external,
            internal_client: client.into(),
            internal_port: internal,
            description: description.into(),
        }
    }

    const ME: Ipv4Addr = Ipv4Addr::new(192, 168, 0, 10);

    /// 실측 표의 모양 — 같은 받는 포트에 갱신이 만든 칸이 여럿, 꺼진 앱의 포트, 남의 기기·남의 이름.
    fn table() -> Vec<Mapping> {
        vec![
            m(57370, "192.168.0.2", 57367, "iC57370"),
            m(49898, "192.168.0.8", 65081, DESCRIPTION),
            m(39050, "192.168.0.10", 49814, DESCRIPTION),
            m(37438, "192.168.0.10", 49814, DESCRIPTION),
            m(53952, "192.168.0.10", 51347, DESCRIPTION),
            m(45381, "192.168.0.10", 53151, DESCRIPTION),
            m(61521, "192.168.0.10", 64779, DESCRIPTION),
            m(36224, "192.168.0.10", 53151, DESCRIPTION),
            Mapping { udp: false, ..m(40000, "192.168.0.10", 49814, DESCRIPTION) },
        ]
    }

    #[test]
    fn keeps_the_advertised_mapping_and_mappings_still_held_by_others() {
        // 64779 는 이 기기의 다른 프로세스가 아직 쥐고 있다.
        let doomed = stale(&table(), ME, Some(53151), &[45381], |p| p == 64779);
        assert_eq!(doomed, [36224, 39050, 37438, 53952]);
    }

    #[test]
    fn without_an_advertised_port_keeps_the_newest_of_the_live_port() {
        let doomed = stale(&table(), ME, Some(53151), &[], |_| false);
        assert!(doomed.contains(&45381) && !doomed.contains(&36224));
        assert!(doomed.contains(&61521));
    }

    #[test]
    fn never_touches_other_devices_other_names_or_tcp() {
        let doomed = stale(&table(), ME, None, &[], |_| false);
        assert!(!doomed.contains(&57370) && !doomed.contains(&49898) && !doomed.contains(&40000));
        assert_eq!(doomed.len(), 6);
    }

    #[test]
    fn a_held_port_is_asked_once() {
        let asked = std::cell::Cell::new(0);
        let doomed = stale(&table(), ME, Some(53151), &[45381], |_| {
            asked.set(asked.get() + 1);
            true
        });
        assert_eq!(doomed, [36224]);
        assert_eq!(asked.get(), 3);
    }

    #[test]
    fn port_in_use_sees_a_bound_socket() {
        let held = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(port_in_use(port));
        drop(held);
        assert!(!port_in_use(port));
    }
}
