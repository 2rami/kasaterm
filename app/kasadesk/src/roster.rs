//! 기기 명부 — 이 기기 본 앱에 물어(`/machines` 와 기기마다 루프백 base 의 `/version`) 볼 수 있는 기기와 그 카사넷 주소를 배운다.
//! 본 앱이 없거나 그 기기 길(ssh 터널)이 끊겨도 보이게 지난 명부를 상태 폴더에 남긴다 — 주소가 바뀌었어도 같은 중계를 홈으로
//! 쓰면 중계 신호로 다시 찾는다(`docs/screen-service-split.md` 「앱이 없을 때 — 찾기」).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

const TIMEOUT: Duration = Duration::from_secs(3);
/// 응답 상한 — 명부·`/version` 은 몇 KB 다.
const BODY_MAX: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Mini,
    Laptop,
    Desktop,
}

impl DeviceKind {
    fn hint(name: &str) -> Option<Self> {
        let name = name.to_lowercase();
        let words = name.split(|c: char| !c.is_alphanumeric());
        if name.contains("windows") || name.contains("windesk") || name.contains("윈도우")
            || words.clone().any(|word| matches!(word, "win" | "win11" | "win10" | "desktop"))
        {
            Some(Self::Desktop)
        } else if name.contains("macbook") || name.contains("맥북") || name == "laptop" {
            Some(Self::Laptop)
        } else if name.contains("macmini") || name.contains("mac mini") || name.contains("맥미니") || name == "mini" {
            Some(Self::Mini)
        } else {
            None
        }
    }

    pub fn classify(label: &str, version: &Value) -> Self {
        [version.get("model"), version.get("os"), version.get("platform")]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find_map(Self::hint)
            .or_else(|| Self::hint(label))
            .unwrap_or(Self::Mini)
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Mini => "mini",
            Self::Laptop => "laptop",
            Self::Desktop => "desktop",
        }
    }
}

/// 명부 한 줄.
#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub label: String,
    pub kind: DeviceKind,
    /// 그 기기 `/version` 의 `kasanet`(`id`·`relay`·`addrs`·`screen`·`screen_node`).
    pub kasanet: Value,
    /// 이번에 본 앱에서 새로 배웠다 — 아니면 지난 명부 그대로다.
    pub fresh: bool,
    /// 관문 계정으로도 보이는 기기의 관문 기기 id — 이 보기 앱 id 를 그 기기에 등록할 때 쓴다(`account::register`).
    pub account: Option<String>,
}

/// 이번에 배운 것.
pub struct Snapshot {
    pub machines: Vec<Machine>,
    /// 이 기기 본 앱이 답했다.
    pub app_alive: bool,
    /// 본 앱 명부에 든 이름 — 그 기기 `/version` 을 못 물었어도 명부에 있으면 지난 주소를 남긴다.
    pub listed: Vec<String>,
}

/// `http://127.0.0.1:<port>` 꼴 base 만 받는다 — 명부의 주소는 본 앱이 든 ssh 터널이고, 루프백 밖을 평문으로 물으면 같은 망의
/// 누구든 주소를 꾸며 넣을 수 있다.
fn loopback(base: &str) -> Option<SocketAddr> {
    let rest = base.trim().trim_end_matches('/').strip_prefix("http://")?;
    let (host, port) = rest.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    matches!(host, "127.0.0.1" | "localhost").then(|| SocketAddr::from(([127, 0, 0, 1], port)))
}

/// 루프백에 HTTP/1.0 GET 하나 — 1.0 이라 덩이 전송 없이 끝까지 읽으면 본문이다.
fn get(base: &str, path: &str) -> Option<Value> {
    let addr = loopback(base)?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).ok()?;
    s.set_read_timeout(Some(TIMEOUT)).ok()?;
    s.set_write_timeout(Some(TIMEOUT)).ok()?;
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nAccept: application/json\r\n\r\n").ok()?;
    let mut buf = Vec::new();
    s.take(BODY_MAX).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n")?;
    head.lines().next().filter(|l| l.split_whitespace().nth(1) == Some("200"))?;
    serde_json::from_str(body).ok()
}

/// 본 앱에 묻는다. 본 앱이 없으면 None.
pub fn fetch(app: &str) -> Option<Snapshot> {
    let own = get(app, "/version")?;
    let own_id = own.pointer("/kasanet/id").and_then(Value::as_str).map(str::to_string);
    let list = get(app, "/machines").unwrap_or(Value::Null);
    let rows = list.as_array().cloned().or_else(|| list.get("machines").and_then(Value::as_array).cloned()).unwrap_or_default();
    let listed = rows.iter().filter_map(|m| Some(m.get("label")?.as_str()?.to_string())).collect();
    let machines = rows
        .iter()
        .filter_map(|m| {
            let label = m.get("label")?.as_str()?.to_string();
            let base = m.get("base")?.as_str()?;
            let version = get(base, "/version")?;
            let kind = DeviceKind::classify(&label, &version);
            let kasanet = version.get("kasanet")?.clone();
            let id = kasanet.get("id").and_then(Value::as_str);
            (id.is_some() && id != own_id.as_deref()).then_some(Machine { label, kind, kasanet, fresh: true, account: None })
        })
        .collect();
    Some(Snapshot { machines, app_alive: true, listed })
}

/// 새로 배운 명부에 지난 명부를 덧댄다 — 이번에 못 물은 기기(그 기기 앱이 꺼졌거나 길이 끊김)는 지난 주소로 남긴다. 본 앱이
/// 답했으면 명부에서 빠진 기기는 버린다.
pub fn merge(fresh: Option<Snapshot>, cached: &[Machine]) -> Snapshot {
    let Some(mut snap) = fresh else {
        let machines = cached.iter().cloned().map(|m| Machine { fresh: false, ..m }).collect();
        return Snapshot { machines, app_alive: false, listed: Vec::new() };
    };
    for old in cached {
        if snap.listed.contains(&old.label) && !snap.machines.iter().any(|m| m.label == old.label) {
            snap.machines.push(Machine { fresh: false, ..old.clone() });
        }
    }
    snap.machines.sort_by(|a, b| a.label.cmp(&b.label));
    snap
}

/// 관문 계정 기기를 덧댄다 — 본 앱 명부에도 있는 기기(같은 카사넷 id)는 그 줄에 관문 id 만 달고, 없는 기기는 관문 이름으로
/// 더한다. 이름이 겹치면 관문 쪽에 「· 계정」을 붙여 가른다(보기 창은 이름으로 기기를 찾는다).
pub fn with_account(snap: &mut Snapshot, account: Vec<Machine>) {
    let id = |m: &Machine| m.kasanet.get("id").and_then(Value::as_str).map(str::to_string);
    for mut a in account {
        if let Some(local) = snap.machines.iter_mut().find(|m| id(m).is_some() && id(m) == id(&a)) {
            local.account = a.account;
            continue;
        }
        if snap.machines.iter().any(|m| m.label == a.label) {
            a.label = format!("{} · 계정", a.label);
        }
        snap.machines.push(a);
    }
    snap.machines.sort_by(|a, b| a.label.cmp(&b.label));
}

pub fn load(path: &PathBuf) -> Vec<Machine> {
    let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return Vec::new();
    };
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let label = m.get("label")?.as_str()?.to_string();
            let kind = m.get("kind").and_then(Value::as_str).and_then(DeviceKind::hint)
                .unwrap_or_else(|| DeviceKind::classify(&label, &Value::Null));
            Some(Machine { label, kind, kasanet: m.get("kasanet")?.clone(), fresh: false, account: None })
        })
        .collect()
}

/// 기기 이름·종류와 공개키·주소만 남긴다.
pub fn save(path: &PathBuf, machines: &[Machine]) {
    let v: Vec<Value> = machines.iter().map(|m| json!({ "label": m.label, "kind": m.kind.tag(), "kasanet": m.kasanet })).collect();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, Value::Array(v).to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(label: &str, fresh: bool) -> Machine {
        Machine { label: label.into(), kind: DeviceKind::Mini, kasanet: json!({ "id": label }), fresh, account: None }
    }

    #[test]
    fn device_kind_uses_explicit_version_hints_and_works_with_old_rosters() {
        assert_eq!(DeviceKind::classify("Studio", &json!({"os": "windows"})), DeviceKind::Desktop);
        assert_eq!(DeviceKind::classify("현장 맥북", &Value::Null), DeviceKind::Laptop);
        assert_eq!(DeviceKind::classify("Studio", &Value::Null), DeviceKind::Mini);
    }

    #[test]
    fn account_machines_join_the_roster_by_kasanet_id() {
        let mut snap = Snapshot { machines: vec![m("미니", true), m("집", true)], app_alive: true, listed: Vec::new() };
        let acct = |label: &str, id: &str, gw: &str| Machine { kasanet: json!({ "id": id }), account: Some(gw.into()), ..m(label, true) };
        with_account(&mut snap, vec![acct("nachoneko", "미니", "g1"), acct("집", "다른 집", "g2"), acct("노트북", "노트북", "g3")]);
        let rows: Vec<(&str, Option<&str>)> = snap.machines.iter().map(|m| (m.label.as_str(), m.account.as_deref())).collect();
        assert_eq!(rows, [("노트북", Some("g3")), ("미니", Some("g1")), ("집", None), ("집 · 계정", Some("g2"))]);
    }

    #[test]
    fn only_loopback_bases_are_asked() {
        assert!(loopback("http://127.0.0.1:18795/").is_some());
        assert!(loopback("http://localhost:8765").is_some());
        assert!(loopback("http://192.168.0.2:8765").is_none());
        assert!(loopback("https://127.0.0.1:8765").is_none());
    }

    #[test]
    fn a_machine_that_did_not_answer_keeps_its_last_address_while_it_is_listed() {
        let cached = [m("집", false), m("미니", false), m("지운 기기", false)];
        let fresh = Snapshot { machines: vec![m("미니", true)], app_alive: true, listed: vec!["집".into(), "미니".into()] };
        let snap = merge(Some(fresh), &cached);
        let names: Vec<(&str, bool)> = snap.machines.iter().map(|m| (m.label.as_str(), m.fresh)).collect();
        assert_eq!(names, [("미니", true), ("집", false)], "명부에서 빠진 기기는 버린다");
        let offline = merge(None, &cached);
        assert!(!offline.app_alive && offline.machines.len() == 3 && offline.machines.iter().all(|m| !m.fresh));
    }
}
