//! 카사브라우저 쪽 창구(`sync.status`·`sync.read`·`sync.apply`·`sync.tabs.put`·`sync.accounts.put`)의 꼴. 브라우저 레포의
//! `internal/browsersync` 가 같은 JSON 을 쓴다. 시험은 [`Browser`] 를 흉내 내고, 앱은 [`super::bridge`] 로 붙는다.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// Go 는 빈 목록을 `null` 로 보낸다(탭·북마크가 하나도 없는 브라우저) — 빈 것으로 받는다.
fn or_empty<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub rev: String,
    #[serde(default)]
    pub collections: BTreeMap<String, String>,
    #[serde(default)]
    pub tabs_rev: String,
    #[serde(default)]
    pub settings_rev: String,
    #[serde(default)]
    pub rooms_rev: String,
}

/// 북마크 또는 폴더(`url` 이 빈 것) 하나. 브라우저의 로컬 id 그대로다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub added: i64,
    #[serde(default, deserialize_with = "or_empty")]
    pub children: Vec<Mark>,
}

impl Mark {
    pub fn folder(&self) -> bool {
        self.url.is_empty()
    }
}

/// 프로필 하나의 북마크. `key` 는 기기 사이의 이름(계정 소문자, 없으면 `default`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub key: String,
    pub rev: String,
    #[serde(default, deserialize_with = "or_empty")]
    pub marks: Vec<Mark>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub group_color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub account: String,
    #[serde(default, deserialize_with = "or_empty")]
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub dropped: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    #[serde(default, deserialize_with = "or_empty")]
    pub collections: Vec<Collection>,
    #[serde(default)]
    pub tabs_rev: String,
    #[serde(default, deserialize_with = "or_empty")]
    pub tabs: Vec<Workspace>,
    /// 기기끼리 나누는 설정(칸 → 값). 옛 브라우저는 없다.
    #[serde(default)]
    pub settings: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    pub settings_rev: String,
    /// 사람의 방. 옛 브라우저는 없다.
    #[serde(default)]
    pub rooms: Option<Rooms>,
}

/// 사람의 방 하나 — 브라우저의 작업공간 id 그대로다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: String,
}

/// 사람의 방, 순서대로. `mode` 가 `rooms` 일 때만 나눌 수 있다 — `accounts` 면 작업공간마다 계정 로그인이 따로라 `list` 가 비어 있다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rooms {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub rev: String,
    #[serde(default, deserialize_with = "or_empty")]
    pub list: Vec<Room>,
}

/// 브라우저에 하는 방 고치기 하나(`add`·`edit`·`remove`·`order`). 더하는 방의 id 는 이쪽이 정한다(동기화 id).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomOp {
    pub op: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// 브라우저에 하는 북마크 고치기 하나. 더하는 항목의 id 는 이쪽이 정한다(동기화 id).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Op {
    pub op: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    pub index: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folder: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub added: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// 다른 기기의 열린 탭. `at` 은 그 기기가 봉인한 시각(밀리초).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub at: u64,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BrowserError {
    /// 브라우저가 꺼져 있다 — 그 바퀴의 북마크·탭은 건너뛴다.
    NotRunning,
    /// 읽은 뒤 사람이 고쳤다(`SYNC_STALE`) — 아무것도 안 바뀌었고 다음 바퀴에 다시 읽는다.
    Stale,
    Failed(String),
}

pub trait Browser {
    fn status(&self) -> Result<Status, BrowserError>;
    fn read(&self) -> Result<Read, BrowserError>;
    /// 새 rev 를 준다.
    fn apply(&self, collection: &str, base_rev: &str, ops: &[Op]) -> Result<String, BrowserError>;
    fn put_tabs(&self, devices: &[Device]) -> Result<(), BrowserError>;
    /// 관문에는 모음이 있는데 이 기기 브라우저에는 없는 계정마다 빈 계정 폴더를 만들게 한다. 만든 계정을 준다.
    /// 옛 브라우저는 모르는 명령이라 `Failed` 다.
    fn put_accounts(&self, accounts: &[String]) -> Result<Vec<String>, BrowserError>;
    /// 다른 기기가 바꾼 설정 칸을 한꺼번에 넣는다. 새 rev 를 준다.
    fn put_settings(&self, base_rev: &str, values: &BTreeMap<String, serde_json::Value>) -> Result<String, BrowserError>;
    /// 방 고치기를 한 묶음으로 한다. 새 rev 를 준다.
    fn apply_rooms(&self, base_rev: &str, ops: &[RoomOp]) -> Result<String, BrowserError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_lists_go_sends_as_null_read_as_empty() {
        let read: Read = serde_json::from_str(r#"{"collections":[{"key":"default","rev":"e.0","marks":null}],"tabs":null,"tabs_rev":"x"}"#).unwrap();
        assert!(read.tabs.is_empty() && read.collections[0].marks.is_empty());
    }
}
