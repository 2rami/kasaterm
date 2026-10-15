//! 나쵸 봇이 띄운 칸의 표지 — 슬랙·디코·앱 요청마다 뜬 칸이 학생 칸과 한눈에 갈리게. 숨기지 않고
//! 표시만 다르게 하는 재료라 그리는 쪽(사이드바·칸 탭·웹 목록)이 같은 값을 읽는다.
//!
//! 근거는 둘이다. 나쵸가 띄운 뒤 직접 붙이는 표지(`surface.set_nacho` — 어느 창구·채널인지는 이것만
//! 안다)와, 나쵸 방(이름 「나쵸」)에서 하네스가 도는 칸. 뒤엣것은 창구를 모르니 나쵸 색만 입힌다.
//!
//! 붙인 표지는 칸 번호가 아니라 칸 열쇠(`surface_keys`)에 묶는다. 나쵸는 스레드가 끝나면 칸을 닫고 새
//! 칸이 그 번호를 물려받는데, 번호에 묶으면 다음에 열린 빈 셸이 슬랙 칸으로 뜬다. 열쇠는 새 칸마다
//! 새로 나므로 옮겨붙지 않고, 저장본에 열쇠와 함께 실려 재시작에도 남는다.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

/// 나쵸가 칸을 모아 두는 방 이름(nacho-neko `NACHO_PANES_ROOM` 기본값).
pub const ROOM_NAME: &str = "나쵸";

/// 채널 이름의 글자 상한 — 칸 탭·목록 한 줄에 서는 말이라 길 이유가 없다.
const PLACE_MAX: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Slack,
    Discord,
    App,
    /// 창구 하나에 묶이지 않은 나쵸 칸(메인 「나쵸 · 일」·「나쵸 · 개인」, 나쵸 방에서 본 칸).
    Main,
}

impl Via {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "slack" => Some(Self::Slack),
            "discord" => Some(Self::Discord),
            "app" => Some(Self::App),
            "main" => Some(Self::Main),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Discord => "discord",
            Self::App => "app",
            Self::Main => "main",
        }
    }

    /// 창구 아이콘 — 앱·웹 아이콘 등록부의 이름. 창구가 없는 칸은 하네스 로고에 나쵸 색만 입힌다.
    pub fn icon(self) -> Option<&'static str> {
        match self {
            Self::Slack => Some("slack"),
            Self::Discord => Some("discord"),
            Self::App => Some("smartphone"),
            Self::Main => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub via: Via,
    /// 사람이 읽는 채널 이름(`#dev-kasa`·디코 채널). 비어도 된다.
    pub place: String,
}

impl Badge {
    pub fn main() -> Self {
        Self { via: Via::Main, place: String::new() }
    }

    pub fn to_json(&self) -> Value {
        json!({ "via": self.via.as_str(), "place": self.place, "icon": self.via.icon() })
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let via = Via::parse(v.get("via")?.as_str()?)?;
        Some(Self { via, place: clean_place(v.get("place").and_then(Value::as_str).unwrap_or("")) })
    }
}

/// `via` 가 `none`·빈 값이면 지우기(`Ok(None)`). 모르는 창구는 거절한다 — 조용히 받아 두면 그리는 쪽이
/// 아무것도 안 그려 「붙였는데 안 보인다」가 된다.
pub fn parse(via: &str, place: &str) -> Result<Option<Badge>, String> {
    let via = via.trim();
    if via.is_empty() || via.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let via = Via::parse(via).ok_or_else(|| format!("모르는 창구 {via:?} — slack·discord·app·main·none"))?;
    Ok(Some(Badge { via, place: clean_place(place) }))
}

fn clean_place(place: &str) -> String {
    place.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(PLACE_MAX).collect()
}

/// 방 이름이 나쵸 방인가 — 사람이 붙인 이름 앞뒤 공백은 무시한다.
pub fn is_room(name: &str) -> bool {
    name.trim() == ROOM_NAME
}

fn registry() -> &'static Mutex<HashMap<String, Badge>> {
    static BADGES: OnceLock<Mutex<HashMap<String, Badge>>> = OnceLock::new();
    BADGES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn room_panes() -> &'static Mutex<HashSet<String>> {
    static ROOM: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ROOM.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `None` 이면 지운다. 열쇠가 아직 없는 칸이면 지금 세운다 — 그 열쇠가 저장본에 같이 실린다.
pub fn set(surface: &str, badge: Option<Badge>) {
    let key = crate::surface_keys::ensure(surface);
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    match badge {
        Some(b) => {
            map.insert(key, b);
        }
        None => {
            map.remove(&key);
        }
    }
}

/// 붙인 표지만 — 방으로 미루어 본 것은 빼고.
pub fn get(surface: &str) -> Option<Badge> {
    let key = crate::surface_keys::get(surface)?;
    registry().lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
}

/// 지금 나쵸 방에 든 칸(탭 포함) — 방 이름과 칸 배치를 아는 GUI 가 바뀔 때마다 통째로 갈아 끼운다.
pub fn set_room_panes(panes: HashSet<String>) {
    let mut cur = room_panes().lock().unwrap_or_else(|e| e.into_inner());
    if *cur != panes {
        *cur = panes;
    }
}

pub fn in_room(surface: &str) -> bool {
    room_panes().lock().unwrap_or_else(|e| e.into_inner()).contains(surface)
}

/// 이 칸을 나쵸 칸으로 그릴 표지. 붙인 것이 먼저고, 없으면 나쵸 방에서 하네스가 도는 칸만 — 방의 바탕
/// 빈 셸까지 나쵸 색이면 「어느 게 나쵸 일인가」가 다시 흐려진다.
pub fn badge(surface: &str, harness_running: bool) -> Option<Badge> {
    get(surface).or_else(|| (harness_running && in_room(surface)).then(Badge::main))
}

/// 저장본 한 칸 기록에 싣는다. 붙인 표지가 없으면 칸을 안 남긴다(방에서 미룬 것은 다시 미룬다).
pub fn save_into(record: &mut serde_json::Map<String, Value>, surface: &str) {
    if let Some(b) = get(surface) {
        record.insert("nacho".into(), b.to_json());
    }
}

/// 저장본에서 되살린다 — 열쇠 복원(`surface_keys::set`) 뒤에 부른다.
pub fn restore_from(record: &Value, surface: &str) {
    if let Some(b) = record.get("nacho").and_then(Badge::from_json) {
        set(surface, Some(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reused_pane_number_does_not_inherit_badge() {
        let id = "%nacho-reuse";
        set(id, parse("slack", "#dev").unwrap());
        assert_eq!(get(id).map(|b| b.via), Some(Via::Slack));
        crate::surface_keys::remove(id);
        assert_eq!(get(id), None);
        crate::surface_keys::ensure(id);
        assert_eq!(get(id), None);
    }

    #[test]
    fn none_clears_and_unknown_is_refused() {
        let id = "%nacho-clear";
        set(id, parse("discord", "일반").unwrap());
        set(id, parse("none", "").unwrap());
        assert_eq!(get(id), None);
        assert!(parse("telegram", "").is_err());
    }

    #[test]
    fn place_is_cleaned_and_capped() {
        let b = parse("SLACK", " #dev\u{7}-kasa \n").unwrap().unwrap();
        assert_eq!((b.via, b.place.as_str()), (Via::Slack, "#dev-kasa"));
        let long = parse("app", &"가".repeat(200)).unwrap().unwrap();
        assert_eq!(long.place.chars().count(), PLACE_MAX);
    }

    #[test]
    fn record_round_trip_follows_surface_key() {
        let id = "%nacho-save";
        set(id, parse("slack", "#ops").unwrap());
        let mut record = serde_json::Map::new();
        record.insert("surface_key".into(), json!(crate::surface_keys::get(id).unwrap()));
        save_into(&mut record, id);
        assert_eq!(record["nacho"]["icon"], "slack");
        let record = Value::Object(record);
        crate::surface_keys::remove(id);
        let restored = "%nacho-restored";
        crate::surface_keys::set(restored, record["surface_key"].as_str().unwrap());
        restore_from(&record, restored);
        assert_eq!(get(restored).map(|b| b.place), Some("#ops".to_string()));
    }

    #[test]
    fn room_marks_only_running_harness_and_set_badge_wins() {
        let (shell, agent, tagged) = ("%nacho-room-shell", "%nacho-room-agent", "%nacho-room-tagged");
        set_room_panes([shell, agent, tagged].map(String::from).into_iter().collect());
        assert_eq!(badge(shell, false), None, "나쵸 방의 바탕 빈 셸은 표지 없음");
        assert_eq!(badge(agent, true), Some(Badge::main()));
        set(tagged, parse("slack", "#dev").unwrap());
        assert_eq!(badge(tagged, true).map(|b| b.via), Some(Via::Slack));
        assert_eq!(badge("%nacho-room-elsewhere", true), None);
        assert!(is_room(" 나쵸 ") && !is_room("나쵸 · 일"));
    }

    #[test]
    fn main_has_no_channel_icon() {
        assert_eq!(Via::Main.icon(), None);
        assert_eq!(parse("main", "").unwrap().unwrap().to_json()["icon"], Value::Null);
    }
}
