//! `/version` 이 싣는 `kasanet` 칸 `{id, relay, addrs, port}` → 상대 주소와 받는 포트. 화면 서비스 노드(`screen_node`)는
//! 받는 포트가 없어 주소만 읽는다(`addr_from_json`).

use std::net::SocketAddr;
use std::str::FromStr;

use iroh::{EndpointAddr, EndpointId, RelayUrl};
use serde_json::Value;

pub fn from_json(v: &Value) -> Option<(EndpointAddr, u16)> {
    let port = u16::try_from(v.get("port")?.as_u64()?)
        .ok()
        .filter(|p| *p != 0)?;
    Some((addr_from_json(v)?, port))
}

/// `{id, relay?, addrs?}` → 상대 주소.
pub fn addr_from_json(v: &Value) -> Option<EndpointAddr> {
    let id = EndpointId::from_str(v.get("id")?.as_str()?).ok()?;
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
    Some(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn peer_needs_id_and_port() {
        let id = SecretKey::generate().public().to_string();
        let v = serde_json::json!({"id": id, "port": 8765, "addrs": ["1.2.3.4:5", "bad"], "relay": "https://r.example./"});
        let (addr, port) = from_json(&v).unwrap();
        assert_eq!(port, 8765);
        assert_eq!(addr.ip_addrs().count(), 1);
        assert_eq!(addr.relay_urls().count(), 1);
        assert!(from_json(&serde_json::json!({"id": id})).is_none());
        assert!(from_json(&serde_json::json!({"id": id, "port": 0})).is_none());
        assert!(from_json(&serde_json::json!({"id": "nope", "port": 1})).is_none());
        assert_eq!(addr_from_json(&serde_json::json!({"id": id, "addrs": ["1.2.3.4:5"]})).map(|a| a.ip_addrs().count()), Some(1), "화면 서비스 노드는 포트 없이");
    }
}
