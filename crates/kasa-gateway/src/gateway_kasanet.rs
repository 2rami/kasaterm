//! `POST /relay/kasanet/ids` — 로그인한 기기가 자기 카사넷 id(앱·보기 앱·화면 서비스, 데스크톱이 관문으로 확인해 받은 폰·보기 앱)를
//! 알린다. 관문은 폐기 안 된 기기의 id 를 한 파일(`KASA_RELAY_KASANET_ALLOW`)로 내놓고, 같은 서버의 국내 중계 접근 확인
//! (`tools/kasanet-relay/access.py`)이 손으로 적는 목록과 함께 그 파일도 읽는다 — 계정으로 로그인한 기기는 `relay.sh allow` 없이
//! 국내 중계를 쓴다(`docs/kasanet.md` 「국내 중계」).
//!
//! - 몸통 `{ids:[64자 16진]}` 이 그 기기의 목록을 통째로 갈아 끼운다. 기기마다 16개, 계정 전체 64개까지 — 넘으면 413.
//! - 기기가 폐기되면(로그아웃·끊기) 그 id 는 곧바로 내보낸 파일에서 빠진다. 계정이 꺼져도 빠진다.
//! - 내보낸 파일은 id 만 싣는다(공개키라 비밀이 아니다) — 계정·기기 이름은 관문 상태 파일(0600)에만 둔다.

use super::*;
use serde::Deserialize;

const MAX_PER_DEVICE: usize = 16;
const MAX_PER_ACCOUNT: usize = 64;
const ALLOW_ENV: &str = "KASA_RELAY_KASANET_ALLOW";

pub(super) fn routes() -> Router<Gate> {
    Router::new().route("/relay/kasanet/ids", axum::routing::post(report))
}

/// 기기 id → 그 기기가 알린 카사넷 id 들.
pub(super) struct Ids {
    path: Option<PathBuf>,
    export: Option<PathBuf>,
    map: Mutex<HashMap<String, Vec<String>>>,
}

impl Ids {
    pub(super) fn open(state: Option<&std::path::Path>) -> Self {
        let path = state.map(|p| p.with_file_name("relay-kasanet-ids.json"));
        let map = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let export = std::env::var_os(ALLOW_ENV).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self { path, export, map: Mutex::new(map) }
    }

    #[cfg(test)]
    pub(super) fn exporting_to(export: &std::path::Path) -> Self {
        Self { path: None, export: Some(export.to_path_buf()), map: Mutex::default() }
    }

    fn save(&self, map: &HashMap<String, Vec<String>>) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, &serde_json::to_vec(map).unwrap_or_default(), 0o600) {
            eprintln!("[gateway] 카사넷 id 를 못 남겼다: {e}");
        }
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn write_atomic(path: &std::path::Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&tmp, path)
}

impl Gate {
    /// 살아 있는 기기의 카사넷 id 를 중계 접근 확인이 읽는 파일로 내놓는다(한 줄에 하나). 파일 자리가 없으면 아무것도 안 한다.
    pub(super) fn export_kasanet_ids(&self) {
        let Some(export) = &self.kasanet_ids.export else { return };
        let live: std::collections::BTreeSet<String> = {
            let devices = self.devices.lock().unwrap();
            let map = self.kasanet_ids.map.lock().unwrap();
            map.iter()
                .filter(|(device, _)| {
                    devices.get(*device).is_some_and(|d| d.revoked_at.is_none() && self.account_active(&d.account))
                })
                .flat_map(|(_, ids)| ids.iter().cloned())
                .collect()
        };
        let mut text = String::from("# 관문이 쓰는 파일 — 계정으로 로그인한 기기의 카사넷 id(gateway_kasanet.rs). 손으로 고치지 않는다.\n");
        for id in live {
            text.push_str(&id);
            text.push('\n');
        }
        if let Err(e) = write_atomic(export, text.as_bytes(), 0o644) {
            eprintln!("[gateway] 중계 허용 파일을 못 썼다({}): {e}", export.display());
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    ids: Vec<String>,
}

async fn report(State(gate): State<Gate>, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
    let Some((device, rec)) = gate.device_of(&headers) else {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    };
    let Ok(Report { ids }) = serde_json::from_slice::<Report>(&body) else {
        return json_err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let mut ids: Vec<String> = ids.into_iter().map(|id| id.trim().to_ascii_lowercase()).collect();
    ids.sort();
    ids.dedup();
    if !ids.iter().all(|id| valid_id(id)) {
        return json_err(StatusCode::BAD_REQUEST, "bad_id");
    }
    if ids.len() > MAX_PER_DEVICE {
        return json_err(StatusCode::PAYLOAD_TOO_LARGE, "too_many_ids");
    }
    let changed = {
        let devices = gate.devices.lock().unwrap();
        let mut map = gate.kasanet_ids.map.lock().unwrap();
        let others: usize = map
            .iter()
            .filter(|(other, _)| *other != &device && devices.get(*other).is_some_and(|d| d.account == rec.account && d.revoked_at.is_none()))
            .map(|(_, list)| list.len())
            .sum();
        if others + ids.len() > MAX_PER_ACCOUNT {
            return json_err(StatusCode::PAYLOAD_TOO_LARGE, "too_many_ids");
        }
        let changed = map.get(&device) != Some(&ids);
        if changed {
            if ids.is_empty() {
                map.remove(&device);
            } else {
                map.insert(device.clone(), ids.clone());
            }
            gate.kasanet_ids.save(&map);
        }
        changed
    };
    if changed {
        gate.export_kasanet_ids();
    }
    axum::Json(serde_json::json!({ "ok": true, "ids": ids.len() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lowercase_hex_endpoint_ids_pass() {
        assert!(valid_id(&"a".repeat(64)));
        assert!(!valid_id(&"a".repeat(63)));
        assert!(!valid_id(&"g".repeat(64)));
        assert!(!valid_id(&"A".repeat(64)));
    }
}
