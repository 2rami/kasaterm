//! 칸 하나의 「지금」 — 오른쪽 Info 열 맨 위 카드. 원본 기기 앱이 칸의 사실을 짓고(`set_provider`), 그 칸을
//! 거울로 연 기기는 `GET /term/pane-info` 를 길게 쥐어 받는다. 신원 확인은 Git 열(`git_panel`)과 같다 — 기계 id·
//! 칸·surface_key 가 지금 원본과 맞아야 답한다.
//!
//! 쥐는 동안 그 칸의 mod 사실 판(`claude_mod::seq`)이 바뀌면 바로, 프로세스·포트가 바뀌면 1초 안에 답한다.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const SCHEMA: &str = "kasa.pane-info.v1";
/// 원격 GET 이 10초에 끊고, 관문 우회는 답 머리를 20초까지만 기다린다 — Git 열과 같은 한도.
pub const WAIT_CAP_MS: u64 = 8000;
/// mod 신호가 없는 변화(프로세스가 뜨고 짐·포트)를 쥔 채로 다시 보는 간격.
const RECHECK: Duration = Duration::from_secs(1);

/// 칸 id → (카드, 판 지문). 지문은 나이(경과 시간)를 뺀 사실이 같으면 같다 — 같으면 쥐고 있는다.
type Provider = Arc<dyn Fn(&str) -> Option<(Value, u64)> + Send + Sync>;
static PROVIDER: OnceLock<Provider> = OnceLock::new();

/// 앱이 한 번 건다. 막는 일(프로세스 표·포트)을 하므로 비동기 일꾼 밖에서 부른다.
pub fn set_provider(provider: impl Fn(&str) -> Option<(Value, u64)> + Send + Sync + 'static) {
    let _ = PROVIDER.set(Arc::new(provider));
}

fn source_pane(query: &HashMap<String, String>) -> Result<String, &'static str> {
    let machine_id = crate::mobile::machine_identity().ok_or("source_missing")?;
    if query.get("machine_id").is_some_and(|id| !id.is_empty() && *id != machine_id) {
        return Err("source_changed");
    }
    let pane = query.get("pane").filter(|p| !p.is_empty()).ok_or("source_missing")?;
    let key = query.get("surface_key").filter(|k| !k.is_empty()).ok_or("update_needed")?;
    if crate::surface_keys::get(pane).as_ref() != Some(key) {
        return Err("source_changed");
    }
    if crate::remote::remote_info(pane).is_some() {
        return Err("mirror_source");
    }
    Ok(pane.clone())
}

async fn observe(pane: &str) -> Option<(Value, u64)> {
    let provider = PROVIDER.get()?.clone();
    let pane = pane.to_string();
    tokio::task::spawn_blocking(move || provider(&pane)).await.ok().flatten()
}

/// `since` 는 직전 답의 `seq`. 없거나 지금과 다르면 바로, 같으면 바뀌거나 `wait_ms`(최대 8초)가 지날 때 답한다.
/// 답 `{schema, ok, seq, card}` 또는 `{schema, ok: false, error}`.
pub async fn wait(query: HashMap<String, String>) -> Value {
    let error = |code: &str| json!({"schema": SCHEMA, "ok": false, "error": code});
    if query.get("schema").map(String::as_str) != Some(SCHEMA) {
        return error("update_needed");
    }
    let pane = match source_pane(&query) {
        Ok(pane) => pane,
        Err(code) => return error(code),
    };
    let since = query.get("since").cloned();
    let hold = query.get("wait_ms").and_then(|s| s.parse::<u64>().ok()).unwrap_or(WAIT_CAP_MS).min(WAIT_CAP_MS);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(hold);
    loop {
        let mod_seq = crate::claude_mod::seq(&pane);
        let Some((card, print)) = observe(&pane).await else { return error("pane_gone") };
        let seq = format!("{mod_seq}.{print:x}");
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if since.as_deref() != Some(seq.as_str()) || left.is_zero() {
            return json!({"schema": SCHEMA, "ok": true, "seq": seq, "card": card});
        }
        crate::claude_mod::wait_seq(&pane, mod_seq, left.min(RECHECK)).await;
    }
}
