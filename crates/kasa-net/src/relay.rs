//! 국내 자체 중계 — 직통이 안 서는 자리(셀룰러 폰·이중 NAT 집 망)에서 데이터를 싣는 중계. 설계 `docs/kasanet.md` 「국내 중계」.
//!
//! n0 공용 중계는 가장 가까운 것(aps1)이 왕복 170ms 라 ssh·관문보다 느려 데이터를 싣지 않는다. 국내 중계는 왕복 10ms 대라
//! 싣는다(`RelayTrust`). 지도에는 n0 도 같이 둔다 — iroh 는 잰 지연으로 홈 중계를 고르니 국내 중계가 홈이 되고, 국내 중계가
//! 죽으면 n0 가 구멍 뚫기 신호를 잇는다.
//!
//! 국내 중계는 허용 목록에 든 EndpointId 만 받는다. 목록에 없는 기기는 홈 중계 접속이 거절된 채 같은 중계를 계속 두드려
//! 구멍 뚫기 신호까지 잃으므로, 거절을 보면 지도에서 빼 n0 로 돌아간다 — 그 기기는 예전과 똑같이 동작한다.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use iroh::{Endpoint, RelayConfig, RelayMode, RelayUrl, Watcher};

/// 네이버 클라우드 서울(KR-2) Micro 서버의 iroh-relay. 운영은 `tools/kasanet-relay/`.
pub const KASA_RELAY: &str = "https://relay-kr.debimarlene.com/";
/// 우리 중계 주소(쉼표로)를 바꾼다. 빈 값·`off` 면 우리 중계 없이 n0 만 — 이 중계를 넣기 전과 같다.
pub const RELAYS_ENV: &str = "KASATERM_KASANET_TRUSTED_RELAYS";
/// 거절당해 뺀 중계를 다시 넣어 보는 간격 — 허용 목록에 막 든 기기가 앱을 다시 켜지 않아도 옮겨 가게.
#[cfg(not(test))]
const RETRY_DENIED: Duration = Duration::from_secs(30 * 60);
#[cfg(test)]
const RETRY_DENIED: Duration = Duration::from_millis(300);

pub fn own_relays() -> Vec<RelayUrl> {
    match std::env::var(RELAYS_ENV) {
        Ok(v) => parse(&v),
        Err(_) => vec![RelayUrl::from_str(KASA_RELAY).expect("KASA_RELAY")],
    }
}

fn parse(v: &str) -> Vec<RelayUrl> {
    if matches!(v.trim(), "off" | "0" | "false") {
        return Vec::new();
    }
    v.split(',')
        .filter_map(|u| RelayUrl::from_str(u.trim()).ok())
        .collect()
}

/// 우리 중계 + n0 기본 지도. 우리 중계가 없으면 n0 기본 그대로. 우리 중계도 QUIC 주소 찾기(UDP 7842)를 켠다 —
/// 그것이 없으면 공인 주소를 못 배워 직통 후보가 사라진다(2026-09-29 터널 너머 중계 실측).
pub fn relay_mode(own: &[RelayUrl]) -> RelayMode {
    if own.is_empty() {
        return RelayMode::Default;
    }
    let map = iroh::defaults::prod::default_relay_map();
    for url in own {
        map.insert(url.clone(), Arc::new(RelayConfig::from(url.clone())));
    }
    RelayMode::Custom(map)
}

/// 우리 중계가 이 기기를 거절하면 지도에서 빼고 `RETRY_DENIED` 뒤 다시 넣는다. 엔드포인트가 닫히면 끝난다.
/// `log` 는 거절·복귀를 사람이 읽을 한 줄로 받는다.
pub async fn fall_back_when_denied(
    endpoint: Endpoint,
    own: Vec<RelayUrl>,
    log: impl Fn(String) + Send + Sync + 'static,
) {
    let log = Arc::new(log);
    let mut status = endpoint.home_relay_status();
    loop {
        let denied: Vec<(RelayUrl, String)> = status
            .get()
            .iter()
            .filter(|s| own.contains(s.url()))
            .filter_map(|s| Some((s.url().clone(), s.auth_denied_reason()?.to_string())))
            .collect();
        for (url, why) in denied {
            let Some(config) = endpoint.remove_relay(&url).await else {
                continue;
            };
            log(format!(
                "국내 중계 {url} 가 이 기기({})를 안 받는다({why}) — 공용 중계로, {}분 뒤 다시",
                endpoint.id(),
                RETRY_DENIED.as_secs() / 60
            ));
            let endpoint = endpoint.clone();
            let log = log.clone();
            tokio::spawn(async move {
                tokio::time::sleep(RETRY_DENIED).await;
                if endpoint.insert_relay(url.clone(), config).await.is_none()
                    && !endpoint.is_closed()
                {
                    log(format!("국내 중계 {url} 다시 넣음"));
                }
            });
        }
        tokio::select! {
            changed = status.updated() => if changed.is_err() { return },
            _ = endpoint.closed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fwd, AllowList, FwdServer, Link, LinkState, RelayTrust, Route};
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use iroh::{EndpointAddr, RelayMap, SecretKey};
    use iroh_relay::server::{Access, AccessControl, ClientRequest};
    use iroh_relay::tls::CaTlsConfig;
    use std::net::Ipv4Addr;
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const T: Duration = Duration::from_secs(20);

    #[test]
    fn env_value_picks_relays() {
        assert!(parse("off").is_empty());
        assert!(parse("").is_empty());
        let two = parse("https://a.example/, https://b.example ,nope");
        assert_eq!(two.len(), 2);
        assert!(matches!(relay_mode(&[]), RelayMode::Default));
        let RelayMode::Custom(map) = relay_mode(&two) else {
            panic!("우리 중계가 있으면 지도를 새로 짠다");
        };
        assert!(map.contains(&two[0]) && map.contains(&two[1]));
        let ap = RelayUrl::from_str("https://aps1-1.relay.n0.iroh.link./").unwrap();
        assert!(map.contains(&ap), "n0 도 그대로 둔다");
        assert!(
            map.get(&two[0]).unwrap().quic.is_some(),
            "QUIC 주소 찾기를 켠다"
        );
    }

    #[derive(Debug)]
    struct Only(Vec<iroh::EndpointId>);

    impl AccessControl for Only {
        async fn on_connect(&self, req: &ClientRequest) -> Access {
            if self.0.contains(&req.endpoint_id()) {
                Access::Allow
            } else {
                // iroh-relay 의 `access.allowlist` 와 같이 까닭 없이 거절한다.
                Access::Deny { reason: None }
            }
        }
    }

    /// 중계만 가진 노드 — IP 전송이 없어 직통이 설 수 없다.
    async fn relay_only(key: SecretKey, allow: &AllowList, map: RelayMap) -> Endpoint {
        crate::builder(presets::Minimal, key, allow)
            .relay_mode(RelayMode::Custom(map))
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .clear_ip_transports()
            .bind()
            .await
            .unwrap()
    }

    async fn wait_until(what: &str, f: impl Fn() -> bool) {
        let start = Instant::now();
        while !f() {
            assert!(start.elapsed() < T, "{what} 이(가) 시간 안에 안 됐다");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trusted_relay_carries_and_untrusted_does_not() {
        let server_key = SecretKey::generate();
        let client_key = SecretKey::generate();
        let (map, url, _relay) = iroh::test_utils::run_relay_server_with_access(
            true,
            Arc::new(Only(vec![server_key.public(), client_key.public()])),
        )
        .await
        .unwrap();

        let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let server_ep = relay_only(
            server_key,
            &AllowList::new([client_key.public()]),
            map.clone(),
        )
        .await;
        server_ep.online().await;
        let server_addr = EndpointAddr::new(server_ep.id()).with_relay_url(url.clone());
        let _server = Router::builder(server_ep)
            .accept(fwd::ALPN, FwdServer::new([echo_port]))
            .spawn();
        let client = relay_only(client_key, &AllowList::default(), map).await;

        // 믿지 않는 중계로는 붙어도 싣지 않는다.
        let plain = Link::start(client.clone(), server_addr.clone(), RelayTrust::default());
        wait_until("중계로 붙음", || plain.state() == LinkState::Relay).await;
        assert!(plain.usable().is_none());
        drop(plain);

        let link = Link::start(client, server_addr, RelayTrust::new([url]));
        wait_until("믿는 중계", || {
            matches!(link.state(), LinkState::TrustedRelay { .. })
        })
        .await;
        let route = Route::direct_only().unwrap();
        route.set_link(link, echo_port);
        let mut s = TcpStream::connect(route.local_addr()).await.unwrap();
        s.write_all(b"hi").await.unwrap();
        let mut got = [0u8; 2];
        tokio::time::timeout(T, s.read_exact(&mut got))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&got, b"hi");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn denied_device_drops_our_relay_and_retries() {
        let (map, url, _relay) =
            iroh::test_utils::run_relay_server_with_access(true, Arc::new(Only(Vec::new())))
                .await
                .unwrap();
        let ep = relay_only(SecretKey::generate(), &AllowList::default(), map).await;
        let said = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        tokio::spawn(fall_back_when_denied(ep.clone(), vec![url.clone()], {
            let said = said.clone();
            move |line| said.lock().unwrap().push(line)
        }));
        wait_until("거절 보고 뺌", || {
            said.lock().unwrap().iter().any(|l| l.contains("안 받는다"))
        })
        .await;
        // 「다시 넣음」은 넣을 때 그 자리가 비어 있어야 나온다 — 거절 뒤 지도에서 빠졌다는 증거다.
        wait_until("다시 넣음", || {
            said.lock().unwrap().iter().any(|l| l.contains("다시 넣음"))
        })
        .await;
        wait_until("다시 거절", || {
            said.lock()
                .unwrap()
                .iter()
                .filter(|l| l.contains("안 받는다"))
                .count()
                >= 2
        })
        .await;
    }
}
