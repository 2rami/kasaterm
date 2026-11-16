//! 이 기기가 마지막으로 맞춘 모습. 평문은 없다 — 항목 이름은 HMAC, 값은 상태 키로 만든 지문뿐이다.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    /// 관문 쪽 차례. 0 이면 아직 관문에 없다.
    pub rev: u64,
    /// 봉인 안의 시각과 기기 — 받은 항목이 이보다 새것일 때만 적용한다.
    pub changed_at_ms: u64,
    pub device: String,
    pub deleted: bool,
    /// `VaultKey::value_mac(아이디\0비밀번호)`. 묘비는 빈 값.
    pub mac: String,
    /// 로컬 항목의 수정 시각 — 같으면 값을 다시 읽지 않는다.
    pub local_ms: Option<u64>,
    /// 받은 묘비를 키체인이 못 지웠을 때 그 로컬 값의 지문. 값이 그대로인 동안은 되살리지 않고 「보류」로 센다.
    #[serde(default)]
    pub held_mac: Option<String>,
    /// 로컬에 못 쓴 관문 항목 — 다음 바퀴에 다시 쓴다. 잠긴 그대로라 평문이 아니다.
    #[serde(default)]
    pub retry_blob: Option<String>,
    /// 못 썼을 때의 로컬 값 지문(없었으면 빈 값). 그사이 사람이 고쳤으면 재시도를 버리고 고친 것을 올린다.
    #[serde(default)]
    pub retry_local: Option<String>,
}

impl Known {
    pub fn version(&self) -> (u64, &str) {
        (self.changed_at_ms, &self.device)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// 이 상태를 만든 기기. 다른 기기에서 복사해 온 상태면 버리고 다시 맞춘다 — 남의 목록과 견주면 다 지운 것으로 보인다.
    #[serde(default)]
    pub device: String,
    pub key_id: String,
    pub revision: u64,
    pub items: BTreeMap<String, Known>,
}

impl State {
    /// 없거나 깨진 파일은 빈 상태로 시작한다 — 다음 바퀴가 관문과 로컬을 처음부터 다시 맞춘다.
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
