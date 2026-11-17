//! 이 기기가 마지막으로 맞춘 모습. 북마크는 동기화 id ↔ 로컬 id 짝과 묶음마다의 판, 내용은 지문만 남긴다.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::record::{MarkRecord, Stamp, TabsRecord};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownMark {
    pub collection: String,
    pub sync_id: String,
    pub local_id: String,
    /// 관문 쪽 차례. 0 이면 아직 관문에 없다.
    pub rev: u64,
    pub folder: bool,
    pub content: Stamp,
    /// 상태 키로 만든 (제목, 주소) 지문.
    pub content_mac: String,
    pub added: i64,
    pub parent: String,
    pub pos: String,
    pub place: Stamp,
    #[serde(default)]
    pub deleted: Option<Stamp>,
}

impl KnownMark {
    pub fn alive(&self) -> bool {
        self.deleted.as_ref().is_none_or(|d| std::cmp::max(&self.content, &self.place) > d)
    }

    /// 내용 없이 판만 든 기록 — 합칠 때 내용은 로컬이나 관문의 것으로 채운다.
    pub fn record(&self, title: String, url: String) -> MarkRecord {
        MarkRecord {
            collection: self.collection.clone(),
            sync_id: self.sync_id.clone(),
            folder: self.folder,
            title,
            url,
            added: self.added,
            content: self.content.clone(),
            parent: self.parent.clone(),
            pos: self.pos.clone(),
            place: self.place.clone(),
            deleted: self.deleted.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// 이 상태를 만든 기기. 다른 기기에서 옮겨 온 상태면 버리고 처음부터 맞춘다.
    #[serde(default)]
    pub device: String,
    pub key_id: String,
    pub revision: u64,
    /// 관문 항목 이름 → 아는 북마크.
    pub marks: BTreeMap<String, KnownMark>,
    /// 한 번이라도 맞춘 모음. 처음 보는 모음이 생기면 그 바퀴는 관문을 처음부터 다시 받는다.
    #[serde(default)]
    pub collections: BTreeSet<String>,
    /// 마지막으로 올린 내 탭의 브라우저 rev 와 그때·관문 차례.
    #[serde(default)]
    pub tabs_sent: String,
    #[serde(default)]
    pub tabs_sent_ms: u64,
    #[serde(default)]
    pub tabs_item_rev: u64,
    /// 마지막으로 브라우저에 넘긴 다른 기기 탭의 지문.
    #[serde(default)]
    pub tabs_shown: String,
    /// 관문 항목 이름 → 다른 기기의 탭(차례, 기록). 브라우저의 `remote-tabs.json` 과 같은 것을 든다.
    #[serde(default)]
    pub remote_tabs: BTreeMap<String, (u64, TabsRecord)>,
}

impl State {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        kasa_identity::relay_auth::write_private(path, &serde_json::to_string(self)?)?;
        Ok(())
    }
}
