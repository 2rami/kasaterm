//! 봉인 안의 평문과 합치는 규칙. 북마크 하나는 내용(제목·주소)·자리(부모·`pos`)·삭제 세 묶음이 저마다 (시각, 기기)를
//! 갖고, 묶음마다 나중 것이 이긴다 — 한 기기에서 이름을 바꾸고 다른 기기에서 옮겨도 둘 다 남는다.

use serde::{Deserialize, Serialize};

use super::browser::Workspace;

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    pub at: u64,
    pub device: String,
}

impl Stamp {
    pub fn new(at: u64, device: &str) -> Self {
        Self { at, device: device.into() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkRecord {
    pub collection: String,
    pub sync_id: String,
    #[serde(default)]
    pub folder: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub added: i64,
    pub content: Stamp,
    /// 부모의 동기화 id. 빈 값은 맨 위.
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub pos: String,
    pub place: Stamp,
    #[serde(default)]
    pub deleted: Option<Stamp>,
}

impl MarkRecord {
    /// 삭제보다 늦은 고침이 있으면 되살아 있다(금고와 같이 나중 것이 이긴다).
    pub fn alive(&self) -> bool {
        self.deleted.as_ref().is_none_or(|d| std::cmp::max(&self.content, &self.place) > d)
    }

    /// 묶음마다 나중 것을 고른다. 같은 항목의 두 판이어야 한다.
    pub fn merge(&self, other: &MarkRecord) -> MarkRecord {
        let mut out = self.clone();
        // 묘비는 내용 없이 같은 내용 판을 든다 — 판이 같으면 내용이 있는 쪽을 남긴다.
        let fills = other.content == self.content && self.title.is_empty() && self.url.is_empty();
        if other.content > self.content || fills {
            out.folder = other.folder;
            out.title = other.title.clone();
            out.url = other.url.clone();
            out.added = other.added;
            out.content = other.content.clone();
        }
        if other.place > self.place {
            out.parent = other.parent.clone();
            out.pos = other.pos.clone();
            out.place = other.place.clone();
        }
        if other.deleted > self.deleted {
            out.deleted = other.deleted.clone();
        }
        out
    }

    /// 가장 늦은 묶음의 시각 — 묘비를 걷을 때와 시각 바닥에 쓴다.
    pub fn latest(&self) -> u64 {
        let mut at = self.content.at.max(self.place.at);
        if let Some(d) = &self.deleted {
            at = at.max(d.at);
        }
        at
    }
}

/// 설정 한 칸: 값과 그 값을 마지막으로 고른 (시각, 기기).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    pub value: serde_json::Value,
    pub stamp: Stamp,
}

/// 기기 하나가 아는 브라우저 설정. 기기마다 제 항목에만 쓰고, 읽는 쪽은 모든 기기의 항목에서 칸마다 나중 것을 고른다 —
/// 한 항목을 여럿이 쓰면 관문 차례가 부딪힐 때 다른 칸의 고침이 묻힌다. `kind` 는 늘 `settings` 다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsRecord {
    pub kind: String,
    pub device: String,
    pub at: u64,
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, Setting>,
}

/// 사람의 방(작업공간) 하나. 북마크처럼 내용(이름·색)·자리·삭제가 저마다 나중 것으로 맞춰진다. `kind` 는 늘 `room` 이다.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomRecord {
    pub kind: String,
    pub sync_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: String,
    pub content: Stamp,
    #[serde(default)]
    pub pos: String,
    pub place: Stamp,
    #[serde(default)]
    pub deleted: Option<Stamp>,
}

impl RoomRecord {
    pub fn alive(&self) -> bool {
        self.deleted.as_ref().is_none_or(|d| std::cmp::max(&self.content, &self.place) > d)
    }

    pub fn merge(&self, other: &RoomRecord) -> RoomRecord {
        let mut out = self.clone();
        if other.content > self.content {
            out.name = other.name.clone();
            out.color = other.color.clone();
            out.content = other.content.clone();
        }
        if other.place > self.place {
            out.pos = other.pos.clone();
            out.place = other.place.clone();
        }
        if other.deleted > self.deleted {
            out.deleted = other.deleted.clone();
        }
        out
    }

    pub fn latest(&self) -> u64 {
        let mut at = self.content.at.max(self.place.at);
        if let Some(d) = &self.deleted {
            at = at.max(d.at);
        }
        at
    }
}

/// 기기 하나의 열린 탭. 그 기기만 쓰므로 부딪히지 않는다. `gone` 은 기기를 뺐을 때의 묘비.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabsRecord {
    pub device: String,
    #[serde(default)]
    pub name: String,
    pub at: u64,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub gone: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(content: u64, place: u64) -> MarkRecord {
        MarkRecord {
            collection: "c".into(),
            sync_id: "m".into(),
            title: format!("t{content}"),
            content: Stamp::new(content, "a"),
            parent: format!("p{place}"),
            place: Stamp::new(place, "a"),
            ..Default::default()
        }
    }

    #[test]
    fn each_group_keeps_its_later_side() {
        let renamed = mark(5, 1);
        let moved = mark(1, 7);
        let both = renamed.merge(&moved);
        assert_eq!((both.title.as_str(), both.parent.as_str()), ("t5", "p7"));
        assert_eq!(both, moved.merge(&renamed), "어느 쪽에서 합쳐도 같다");
    }

    #[test]
    fn a_room_keeps_each_groups_later_side_and_a_later_edit_revives_it() {
        let room = |content: u64, place: u64| RoomRecord {
            kind: "room".into(),
            sync_id: "r".into(),
            name: format!("n{content}"),
            content: Stamp::new(content, "a"),
            pos: format!("p{place}"),
            place: Stamp::new(place, "a"),
            ..Default::default()
        };
        let both = room(5, 1).merge(&room(1, 7));
        assert_eq!((both.name.as_str(), both.pos.as_str()), ("n5", "p7"));
        let mut gone = room(1, 1);
        gone.deleted = Some(Stamp::new(3, "b"));
        assert!(!gone.alive());
        assert!(gone.merge(&room(4, 1)).alive(), "삭제 뒤의 고침이 이긴다");
    }

    #[test]
    fn a_later_edit_brings_a_deleted_mark_back() {
        let mut gone = mark(1, 1);
        gone.deleted = Some(Stamp::new(3, "b"));
        assert!(!gone.alive());
        let edited = gone.merge(&mark(4, 1));
        assert!(edited.alive(), "삭제 뒤의 고침이 이긴다");
        let old_edit = gone.merge(&mark(2, 2));
        assert!(!old_edit.alive(), "삭제 앞의 고침은 못 살린다");
        assert_eq!(gone.merge(&mark(1, 1)).deleted, gone.deleted, "삭제는 지워지지 않는다");
    }
}
