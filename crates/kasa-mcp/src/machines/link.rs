//! 기기마다 닿고 끊긴 순간과 그 이유. 다른 기기가 목록에서 통째로 사라졌는데 왜인지 알 길이
//! 없었다(2026-10-15 지적 「이유라도 알려주게」) — 폴링이 실패 사유를 버리고, 명부에서 빠진 기기는
//! 흔적 없이 지워졌다. 판정은 화면과 같은 규칙(`STALE_AFTER`)이라 기록과 사이드바가 어긋나지 않고,
//! 명부에서 빠진 기기는 이유를 단 채 남는다(`ghosts`).

use super::{Cache, Machine, STALE_AFTER, canonical_roster, seen_for_machine};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 기기마다 남기는 바뀐 순간의 수. 흔들리는 길이라도 하루 안의 일은 대개 이 안에 든다.
const KEEP: usize = 20;

/// 폴링 한 번이 실패한 모양. 사람 말은 길(터널·관문)과 합쳐 `describe` 가 짓는다.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Failure {
    Timeout(String),
    Refused(String),
    /// 연결은 됐는데 답 없이 닫혔다 — ssh 터널은 살아 있고 저쪽 앱이 없을 때의 모양.
    Closed(String),
    Status(u16),
    Body(String),
    Other(String),
}

impl Failure {
    pub(super) fn from_error(error: &reqwest::Error) -> Self {
        let chain = error_chain(error);
        let lower = chain.to_ascii_lowercase();
        if error.is_timeout() || lower.contains("timed out") {
            Self::Timeout(chain)
        } else if lower.contains("connection refused") {
            Self::Refused(chain)
        } else if ["connection closed", "connection reset", "broken pipe", "unexpected eof", "incomplete message"]
            .iter()
            .any(|cue| lower.contains(cue))
        {
            Self::Closed(chain)
        } else {
            Self::Other(chain)
        }
    }

    fn raw(&self) -> String {
        match self {
            Self::Timeout(raw) | Self::Refused(raw) | Self::Closed(raw) | Self::Body(raw) | Self::Other(raw) => raw.clone(),
            Self::Status(code) => format!("HTTP {code}"),
        }
    }

    /// 길 자체가 안 닿은 실패 — 관문 중계가 대신 닿는지 함께 말할 가치가 있는 것.
    pub(super) fn transport(&self) -> bool {
        matches!(self, Self::Timeout(_) | Self::Refused(_) | Self::Closed(_))
    }
}

/// reqwest 오류는 맨 위 줄이 「요청 보내기 실패」뿐이고 진짜 사유(거부·시간 초과)는 source 사슬 끝에 있다.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut next = error.source();
    while let Some(cause) = next {
        let text = cause.to_string();
        if !parts.iter().any(|part| part.contains(&text)) {
            parts.push(text);
        }
        next = cause.source();
    }
    parts.join(": ").chars().take(240).collect()
}

/// 실패를 해석할 때 필요한 그 기기의 길 사정.
#[derive(Clone, Debug, Default)]
pub(super) struct Route {
    /// 이 앱이 든 ssh 터널 뒤의 기기인가.
    pub tunneled: bool,
    pub tunnel_alive: bool,
    /// 죽은 터널 ssh 의 마지막 stderr 한 줄.
    pub tunnel_error: Option<String>,
    /// 관문 우회 길 — None 은 관문을 안 쓰는 기기(로그인 전·id 모름).
    pub relay: Option<bool>,
    /// 명부 주소가 루프백인가 — 거부면 그 너머가 터널인지 앱인지 모른다.
    pub loopback: bool,
    /// 그쪽이 알려 온 기기 — 주소가 그쪽 ssh 가 이 기기에 열어 둔 되돌아오는 길(-R)이다.
    pub guest: bool,
}

/// (사이드바 한 줄에 들어갈 이유, 원문 한 줄).
pub(super) fn describe(failure: &Failure, route: &Route) -> (String, String) {
    let ssh_gone = route.tunneled && !route.tunnel_alive && failure.transport();
    let (mut reason, raw) = if ssh_gone {
        let raw = route.tunnel_error.clone().unwrap_or_else(|| failure.raw());
        let reason = if raw.contains("Permission denied") || raw.contains("Host key verification failed") {
            "ssh 인증 거절"
        } else if raw.contains("Could not resolve hostname") {
            "ssh 주소를 못 찾음"
        } else {
            "ssh 길 끊김"
        };
        (reason.to_string(), raw)
    } else {
        let reason = match failure {
            Failure::Timeout(_) => "응답 없음".to_string(),
            Failure::Refused(_) if route.guest => "그쪽이 열어 둔 되돌아오는 길(-R)이 끊김".to_string(),
            Failure::Refused(_) if route.loopback && !route.tunneled => "연결 거부 · 터널이나 그 기기 앱이 꺼짐".to_string(),
            Failure::Refused(_) | Failure::Closed(_) => "그 기기 앱이 꺼짐·재시작 중".to_string(),
            Failure::Status(401 | 403) => "인증 거절".to_string(),
            Failure::Status(404 | 405) => "옛 판이라 방 목록 길 없음".to_string(),
            Failure::Status(code) => format!("오류 응답 HTTP {code}"),
            Failure::Body(_) => "답 모양이 다름".to_string(),
            Failure::Other(_) => "모름".to_string(),
        };
        (reason, failure.raw())
    };
    if failure.transport() {
        match route.relay {
            Some(true) => reason.push_str(" · 관문 중계로는 닿음"),
            Some(false) if matches!(failure, Failure::Timeout(_)) && !ssh_gone => reason = "직통·중계 둘 다 응답 없음".into(),
            Some(false) => reason.push_str(" · 관문 중계도 안 닿음"),
            None => {}
        }
    }
    (reason, raw)
}

#[derive(Clone, Debug, PartialEq)]
struct Event {
    at_ms: u64,
    up: bool,
    reason: String,
    detail: String,
}

#[derive(Clone, Debug, Default)]
struct Link {
    up: Option<bool>,
    last_ok_ms: Option<u64>,
    reason: String,
    detail: String,
    machine_id: Option<String>,
    guest: bool,
    ssh: Option<String>,
    /// 마지막으로 닿았을 때 그 기기에 열려 있던 방 수 — 끊긴 동안 「다시 붙으면 돌아온다」고 말할 근거.
    rooms: usize,
    /// 명부에서 빠져 이유만 남은 기기. 다시 들어오면 걷힌다.
    gone: bool,
    events: VecDeque<Event>,
}

impl Link {
    fn push(&mut self, up: bool, reason: &str, detail: &str) {
        self.events.push_back(Event { at_ms: now_ms(), up, reason: reason.to_string(), detail: detail.to_string() });
        while self.events.len() > KEEP {
            self.events.pop_front();
        }
    }
}

#[derive(Default)]
struct Table {
    links: HashMap<String, Link>,
    /// 지난 바퀴 목록에 있던 기기 — 이번에 없으면 빠진 것이다.
    listed: HashSet<String>,
    /// 사람이 설정에서 지운 이름 — 유령으로 남기지 않는다.
    forgotten: HashSet<String>,
}

fn table() -> &'static Mutex<Table> {
    static T: OnceLock<Mutex<Table>> = OnceLock::new();
    T.get_or_init(Default::default)
}

fn boot() -> Instant {
    static B: OnceLock<Instant> = OnceLock::new();
    *B.get_or_init(Instant::now)
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// 이 바퀴에 본 기기 하나.
struct Observed {
    label: String,
    online: bool,
    last_ok_ms: Option<u64>,
    rooms: usize,
    machine_id: Option<String>,
    guest: bool,
    ssh: Option<String>,
    base: String,
    tunneled: bool,
}

fn rooms_of(panes: &[Value]) -> usize {
    panes
        .iter()
        .filter(|p| p.get("id").and_then(Value::as_str).is_some_and(|id| id.starts_with('%')))
        .filter(|p| p.get("closed").and_then(Value::as_bool) != Some(true))
        .filter(|p| p.get("mirror_of").is_none_or(Value::is_null))
        .filter_map(|p| p.get("window").and_then(Value::as_u64))
        .collect::<HashSet<_>>()
        .len()
}

fn observe(list: &[Machine], cache: &Cache) -> Vec<Observed> {
    let now = now_ms();
    canonical_roster(list.to_vec(), cache)
        .into_iter()
        .map(|m| {
            let hit = seen_for_machine(&m, cache);
            let age = hit.map(|seen| seen.at.elapsed());
            Observed {
                online: age.is_some_and(|age| age < STALE_AFTER),
                last_ok_ms: age.map(|age| now.saturating_sub(age.as_millis() as u64)),
                rooms: hit.map_or(0, |seen| rooms_of(&seen.panes)),
                machine_id: m.machine_id.clone().or_else(|| hit.and_then(|seen| seen.machine_id.clone())),
                label: m.label,
                guest: m.guest,
                ssh: m.ssh,
                base: m.base,
                tunneled: m.tunneled,
            }
        })
        .collect()
}

fn loopback(base: &str) -> bool {
    let host = base.trim_start_matches("http://").trim_start_matches("https://");
    host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
}

/// 명부에서 빠진 까닭과 원문. 파일을 못 읽은 것과 사람이 지운 것과 손님 기한이 지난 것은 고칠 자리가 다르다.
fn removal_reason(label: &str, guest: bool) -> (String, String) {
    if guest {
        return (format!("알려 오던 길이 {}분 넘게 끊겨 목록에서 빠짐", super::GUEST_FORGET.as_secs() / 60), String::new());
    }
    match super::read_listed() {
        Err(error) => ("명부 파일을 못 읽음".into(), error),
        Ok(entries) if entries.iter().any(|e| entry_label(e) == Some(label) && unresolved(e)) => (UNRESOLVED.into(), String::new()),
        Ok(_) => ("명부(machines.json)에서 빠짐".into(), String::new()),
    }
}

const UNRESOLVED: &str = "계정 동기화로 받은 연결 주소를 이 기기에서 못 풀었음";

fn entry_label(entry: &Value) -> Option<&str> {
    entry.get("label").and_then(Value::as_str).map(str::trim).filter(|l| !l.is_empty())
}

fn unresolved(entry: &Value) -> bool {
    entry.get("account_sync_unresolved").and_then(Value::as_bool) == Some(true)
}

/// 폴링 한 바퀴가 끝난 뒤 — 기기마다 화면과 같은 판정으로 붙음·끊김이 바뀌었으면 적고, 명부에서
/// 빠진 기기는 이유를 달아 남긴다. `failures` 는 이번 바퀴 실패(라벨 → 사유).
pub(super) fn settle(list: &[Machine], failures: &HashMap<String, Failure>) {
    let observed = {
        let Ok(cache) = super::cache().lock() else { return };
        observe(list, &cache)
    };
    // 길 사정은 캐시 잠금 밖에서 — 보드 길 조회가 명부를 다시 읽으며 같은 캐시를 잠근다.
    let routes: HashMap<String, Route> = observed
        .iter()
        .filter(|o| failures.contains_key(&o.label))
        .map(|o| (o.label.clone(), route_of(o)))
        .collect();
    let unresolved_labels: Vec<String> = super::read_listed()
        .unwrap_or_default()
        .iter()
        .filter(|e| unresolved(e))
        .filter_map(|e| entry_label(e).map(str::to_string))
        .collect();
    let Ok(mut table) = table().lock() else { return };
    apply(&mut table, &observed, failures, &routes, &unresolved_labels, boot().elapsed() >= STALE_AFTER);
}

/// 잠금 밖에서 못 읽은 길 사정의 대신 — 명부만으로 아는 것.
fn route_hint(o: &Observed) -> Route {
    Route { tunneled: o.tunneled, loopback: loopback(&o.base), guest: o.guest, ..Route::default() }
}

fn route_of(o: &Observed) -> Route {
    let (tunnel_alive, tunnel_error) = super::tunnel_state(&o.label);
    let relay = o.machine_id.as_deref().and_then(|id| {
        crate::board_service::relay_base(id)?;
        Some(crate::board_service::known_route(id).is_some())
    });
    Route { tunneled: o.tunneled, tunnel_alive, tunnel_error, relay, loopback: loopback(&o.base), guest: o.guest }
}

fn apply(
    table: &mut Table,
    observed: &[Observed],
    failures: &HashMap<String, Failure>,
    routes: &HashMap<String, Route>,
    unresolved_labels: &[String],
    settled: bool,
) {
    let current: HashSet<String> = observed.iter().map(|o| o.label.clone()).collect();
    let live_ids: HashSet<&str> = observed.iter().filter_map(|o| o.machine_id.as_deref()).collect();
    for o in observed {
        table.forgotten.remove(&o.label);
        let link = table.links.entry(o.label.clone()).or_default();
        link.machine_id = o.machine_id.clone().or(link.machine_id.take());
        link.guest = o.guest;
        link.ssh = o.ssh.clone();
        if o.last_ok_ms.is_some() {
            link.last_ok_ms = o.last_ok_ms;
        }
        if o.online {
            link.rooms = o.rooms;
        }
        let returned = std::mem::take(&mut link.gone);
        // 실패는 화면이 끊김으로 바뀌기(20초) 전부터 적어 둔다 — 바뀌는 순간 이유가 함께 서게.
        match failures.get(&o.label) {
            Some(failure) => {
                let route = routes.get(&o.label).cloned().unwrap_or_else(|| route_hint(o));
                (link.reason, link.detail) = describe(failure, &route);
            }
            None if o.online => {
                link.reason.clear();
                link.detail.clear();
            }
            None if link.reason.is_empty() => link.reason = "모름".into(),
            None => {}
        }
        if o.online {
            if link.up != Some(true) {
                link.push(true, if returned { "목록에 다시 들어옴 · 붙음" } else { "붙음" }, "");
                link.up = Some(true);
            }
            continue;
        }
        // 부팅 직후엔 터널이 서기 전이라 누구나 잠깐 안 닿는다 — 첫 판정은 그 시간이 지난 뒤에.
        if link.up == Some(true) || (link.up.is_none() && settled) || returned {
            let (reason, detail) = (link.reason.clone(), link.detail.clone());
            link.push(false, &reason, &detail);
            link.up = Some(false);
        }
    }
    let gone: Vec<String> = table.listed.difference(&current).cloned().collect();
    for label in gone {
        if table.forgotten.contains(&label) {
            table.links.remove(&label);
            continue;
        }
        let Some(link) = table.links.get_mut(&label) else { continue };
        // 같은 기기의 다른 이름(되돌아오는 길로 알려 온 별칭)이 합쳐진 것뿐이면 빠진 게 아니다.
        if link.machine_id.as_deref().is_some_and(|id| live_ids.contains(id)) {
            table.links.remove(&label);
            continue;
        }
        let (reason, detail) = removal_reason(&label, link.guest);
        link.gone = true;
        link.up = Some(false);
        link.push(false, &reason, &detail);
        (link.reason, link.detail) = (reason, detail);
    }
    for label in unresolved_labels {
        if current.contains(label) || table.forgotten.contains(label) {
            continue;
        }
        let link = table.links.entry(label.clone()).or_default();
        if !link.gone || link.reason != UNRESOLVED {
            link.gone = true;
            link.up = Some(false);
            link.reason = UNRESOLVED.into();
            link.detail = "설정 「연결 기기」에서 주소를 넣어 주세요".into();
            link.push(false, UNRESOLVED, "");
        }
    }
    table.listed = current;
}

/// 사람이 설정에서 지우거나 이름을 바꾼 기기 — 「명부에서 빠짐」 유령으로 남기지 않는다.
pub(super) fn forget(labels: impl IntoIterator<Item = String>) {
    let Ok(mut table) = table().lock() else { return };
    for label in labels {
        table.links.remove(&label);
        table.forgotten.insert(label);
    }
}

fn view(link: &Link) -> Value {
    json!({
        "state": match link.up { Some(true) => "up", Some(false) => "down", None => "unknown" },
        "reason": (!link.reason.is_empty()).then_some(&link.reason),
        "detail": (!link.detail.is_empty()).then_some(&link.detail),
        "last_ok_ms": link.last_ok_ms,
        "rooms": link.rooms,
        "gone": link.gone,
        "events": link.events.iter().rev().map(|e| json!({
            "at_ms": e.at_ms, "up": e.up, "reason": e.reason,
            "detail": (!e.detail.is_empty()).then_some(&e.detail),
        })).collect::<Vec<_>>(),
    })
}

/// 스냅샷 행에 붙일 연결 기록. 아직 한 번도 판정 전이면 None.
pub(super) fn view_of(label: &str) -> Option<Value> {
    table().lock().ok()?.links.get(label).map(view)
}

/// 명부에서 빠졌지만 이유를 남긴 기기들 — 스냅샷 끝에 끊긴 행으로 붙는다.
pub(super) fn ghosts() -> Vec<Value> {
    let Ok(table) = table().lock() else { return Vec::new() };
    let mut rows: Vec<(&String, &Link)> = table.links.iter().filter(|(_, link)| link.gone).collect();
    rows.sort_by_key(|(label, _)| label.as_str());
    rows.into_iter()
        .map(|(label, link)| json!({
            "label": label,
            "route": link.machine_id.as_ref().map_or_else(|| label.clone(), |id| format!("~{id}")),
            "base": "",
            "ssh": link.ssh,
            "guest": link.guest,
            "online": false,
            "online_via": null,
            "rtt_ms": null,
            "path": null,
            "path_rtt_ms": null,
            "ago_secs": link.last_ok_ms.map(|at| now_ms().saturating_sub(at) / 1000),
            "sync_capable": true,
            "build": null,
            "build_match": true,
            "panes": [],
            "gone": true,
            "link": view(link),
        }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(label: &str, online: bool, id: Option<&str>) -> Observed {
        Observed {
            label: label.into(),
            online,
            last_ok_ms: online.then(now_ms),
            rooms: 2,
            machine_id: id.map(str::to_string),
            guest: false,
            ssh: None,
            base: "http://127.0.0.1:1".into(),
            tunneled: false,
        }
    }

    fn reasons(table: &Table, label: &str) -> Vec<(bool, String)> {
        table.links[label].events.iter().map(|e| (e.up, e.reason.clone())).collect()
    }

    #[test]
    fn only_transitions_are_recorded_and_the_reason_is_the_cause_at_that_moment() {
        let mut table = Table::default();
        let none = HashMap::new();
        let refused = HashMap::from([("미니".to_string(), Failure::Refused("tcp connect error: Connection refused".into()))]);
        let timeout = HashMap::from([("미니".to_string(), Failure::Timeout("operation timed out".into()))]);
        let routes = HashMap::from([("미니".to_string(), Route { loopback: true, ..Route::default() })]);
        apply(&mut table, &[seen("미니", true, Some("m1"))], &none, &routes, &[], true);
        apply(&mut table, &[seen("미니", true, Some("m1"))], &none, &routes, &[], true);
        apply(&mut table, &[seen("미니", false, Some("m1"))], &refused, &routes, &[], true);
        apply(&mut table, &[seen("미니", false, Some("m1"))], &timeout, &routes, &[], true);
        apply(&mut table, &[seen("미니", true, Some("m1"))], &none, &routes, &[], true);
        assert_eq!(reasons(&table, "미니"), vec![
            (true, "붙음".into()),
            (false, "연결 거부 · 터널이나 그 기기 앱이 꺼짐".into()),
            (true, "붙음".into()),
        ]);
        assert!(table.links["미니"].reason.is_empty(), "다시 붙으면 끊긴 이유는 걷힌다");
        apply(&mut table, &[seen("미니", true, Some("m1"))], &timeout, &routes, &[], true);
        assert_eq!(table.links["미니"].reason, "응답 없음", "아직 붙은 것으로 보이는 동안에도 실패 이유는 미리 선다");
        assert_eq!(table.links["미니"].events.len(), 3, "끊김 판정 전이라 기록은 그대로");
    }

    #[test]
    fn a_machine_that_never_answered_is_judged_only_after_the_boot_grace() {
        let mut table = Table::default();
        let fail = HashMap::from([("PC".to_string(), Failure::Timeout("timed out".into()))]);
        apply(&mut table, &[seen("PC", false, None)], &fail, &HashMap::new(), &[], false);
        assert!(table.links["PC"].events.is_empty());
        assert_eq!(table.links["PC"].reason, "응답 없음", "이유는 판정 전에도 보인다");
        apply(&mut table, &[seen("PC", false, None)], &fail, &HashMap::new(), &[], true);
        assert_eq!(reasons(&table, "PC"), vec![(false, "응답 없음".into())]);
    }

    #[test]
    fn a_vanished_machine_stays_with_its_reason_unless_it_merged_or_was_deleted() {
        let mut table = Table::default();
        let none = HashMap::new();
        let mut guest = seen("손님", true, Some("g1"));
        guest.guest = true;
        apply(&mut table, &[seen("미니", true, Some("m1")), guest, seen("별칭", true, Some("m1x"))], &none, &HashMap::new(), &[], true);
        // 별칭이 미니와 같은 기기로 밝혀져 합쳐진 바퀴 — 미니 쪽 id 가 별칭의 것과 같아진다.
        table.links.get_mut("별칭").unwrap().machine_id = Some("m1".into());
        apply(&mut table, &[seen("미니", true, Some("m1"))], &none, &HashMap::new(), &[], true);
        assert!(!table.links.contains_key("별칭"), "합쳐진 별칭은 유령이 아니다");
        let ghost = &table.links["손님"];
        assert!(ghost.gone);
        assert!(ghost.reason.starts_with("알려 오던 길이"), "{}", ghost.reason);
        // 손님이 다시 알려 오면 유령이 걷히고 「다시 들어옴」이 남는다.
        let mut back = seen("손님", true, Some("g1"));
        back.guest = true;
        apply(&mut table, &[seen("미니", true, Some("m1")), back], &none, &HashMap::new(), &[], true);
        assert!(!table.links["손님"].gone);
        assert_eq!(table.links["손님"].events.back().unwrap().reason, "목록에 다시 들어옴 · 붙음");
    }

    #[test]
    fn deleted_machines_leave_no_ghost() {
        let mut table = Table::default();
        apply(&mut table, &[seen("옛 이름", true, None)], &HashMap::new(), &HashMap::new(), &[], true);
        table.forgotten.insert("옛 이름".into());
        apply(&mut table, &[], &HashMap::new(), &HashMap::new(), &[], true);
        assert!(!table.links.contains_key("옛 이름"));
    }

    #[test]
    fn unresolved_account_sync_entries_show_up_instead_of_vanishing() {
        let mut table = Table::default();
        apply(&mut table, &[], &HashMap::new(), &HashMap::new(), &["데스크탑".to_string()], true);
        apply(&mut table, &[], &HashMap::new(), &HashMap::new(), &["데스크탑".to_string()], true);
        let link = &table.links["데스크탑"];
        assert!(link.gone);
        assert_eq!(link.reason, UNRESOLVED);
        assert_eq!(link.events.len(), 1, "같은 이유를 바퀴마다 다시 적지 않는다");
    }

    #[test]
    fn reasons_follow_the_route_that_actually_failed() {
        let timeout = Failure::Timeout("timed out".into());
        let both_down = Route { relay: Some(false), ..Route::default() };
        assert_eq!(describe(&timeout, &both_down).0, "직통·중계 둘 다 응답 없음");
        let relay_up = Route { relay: Some(true), ..Route::default() };
        assert_eq!(describe(&timeout, &relay_up).0, "응답 없음 · 관문 중계로는 닿음");
        let dead_tunnel = Route {
            tunneled: true,
            tunnel_error: Some("user@host: Permission denied (publickey).".into()),
            ..Route::default()
        };
        let refused = Failure::Refused("Connection refused".into());
        assert_eq!(describe(&refused, &dead_tunnel), ("ssh 인증 거절".into(), "user@host: Permission denied (publickey).".into()));
        let live_tunnel = Route { tunneled: true, tunnel_alive: true, ..Route::default() };
        assert_eq!(describe(&Failure::Closed("connection closed before message completed".into()), &live_tunnel).0,
            "그 기기 앱이 꺼짐·재시작 중");
        assert_eq!(describe(&refused, &Route { guest: true, loopback: true, ..Route::default() }).0,
            "그쪽이 열어 둔 되돌아오는 길(-R)이 끊김");
        assert_eq!(describe(&Failure::Status(403), &Route::default()).0, "인증 거절");
        assert_eq!(describe(&Failure::Status(404), &Route::default()).0, "옛 판이라 방 목록 길 없음");
        assert_eq!(describe(&Failure::Other("tls handshake eof".into()), &Route::default()),
            ("모름".into(), "tls handshake eof".into()));
    }

    #[test]
    fn room_count_ignores_mirrors_closed_and_web_shells() {
        let panes = vec![
            json!({"id": "%1", "window": 0}),
            json!({"id": "%2", "window": 0}),
            json!({"id": "%3", "window": 1, "closed": true}),
            json!({"id": "%4", "window": 2, "mirror_of": "맥북"}),
            json!({"id": "web-1", "window": 3}),
            json!({"id": "%5", "window": 4}),
        ];
        assert_eq!(rooms_of(&panes), 2);
    }
}
