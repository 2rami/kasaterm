//! 기기 목록이 기기마다 지금 쓰는 Claude 계정과 막힌 사유를 보이는 재료.
//!
//! 공통 선택을 못 따라간 기기는 그 기기 설정창을 열기 전엔 어느 계정에 머물러 있는지도, 왜 안 따라갔는지도
//! 보이지 않았다. 다른 기기 값은 그 기기 `/settings/values` 의 계정 칸에서 읽는다 — 기기 팝오버가 열려 있는
//! 동안만, 렌더 스레드 밖에서, 기기마다 간격을 두고.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

const EVERY: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DeviceAccount {
    /// 「Claude · 별명 · 주간 34%」 한 줄.
    pub(crate) line: String,
    /// 다시 로그인이 필요하거나 공통 선택을 못 따라간 사유. 없으면 None.
    pub(crate) warning: Option<String>,
}

/// 설정 값의 Claude 계정 칸 하나를 한 줄로 줄인다. 폰 기기 목록과 같은 재료·같은 순서다.
pub(crate) fn summarize(accounts: &[Value], active: &str, sync: Option<&Value>, shared: Option<&Value>) -> DeviceAccount {
    let text = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let row = accounts.iter().find(|row| row.get("id").and_then(Value::as_str) == Some(active));
    let mut line = match row {
        Some(row) => {
            let name = text(row, "name");
            let email = text(row, "email");
            let mut line = format!("Claude · {}", if name.is_empty() { &email } else { &name });
            if !email.is_empty() && email != name {
                line.push_str(&format!(" · {email}"));
            }
            line
        }
        None if active.is_empty() => "Claude · 기본 로그인".to_string(),
        None => "Claude · 계정 확인 중".to_string(),
    };
    if let Some(pct) = row.and_then(|row| row.get("usage")).and_then(Value::as_f64) {
        let window = row.map(|row| text(row, "usage_label")).unwrap_or_default();
        line.push_str(&format!(" · {}{:.0}%", if window.is_empty() { String::new() } else { format!("{window} ") }, pct));
    }
    let relogin = row.and_then(|row| row.get("relogin")).and_then(Value::as_str).is_some();
    let stopped = sync
        .filter(|sync| sync.get("failed").and_then(Value::as_bool) == Some(true))
        .map(|sync| {
            let details = text(sync, "details");
            let summary = text(sync, "summary");
            if details.is_empty() { summary } else { format!("{summary} · {}", details.lines().next().unwrap_or("")) }
        });
    let diverged = shared
        .filter(|shared| shared.get("follows").and_then(Value::as_bool) == Some(false))
        .map(|shared| format!("공통 선택({})과 다른 계정", text(shared, "name")));
    let warning = match (relogin, stopped, diverged) {
        (_, Some(stopped), _) => Some(stopped),
        (true, None, _) => Some("다시 로그인 필요".to_string()),
        (false, None, diverged) => diverged,
    };
    DeviceAccount { line, warning }
}

struct Entry {
    tried: Instant,
    inflight: bool,
    value: Option<DeviceAccount>,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static C: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 그 기기의 계정 칸을 간격을 두고 읽게 한다. 팝오버가 그릴 때 부르며, 조회는 별도 스레드다.
pub(crate) fn poll(base: &str) {
    if base.is_empty() || crate::verification_run() {
        return;
    }
    let start = {
        let Ok(mut cache) = cache().lock() else { return };
        let entry = cache.entry(base.to_string()).or_insert(Entry { tried: Instant::now() - EVERY, inflight: false, value: None });
        if entry.inflight || entry.tried.elapsed() < EVERY {
            false
        } else {
            entry.inflight = true;
            entry.tried = Instant::now();
            true
        }
    };
    if !start {
        return;
    }
    let base = base.to_string();
    std::thread::spawn(move || {
        let got = kasa_remote::remote::fetch_accounts(&base, None)
            .ok()
            .map(|v| summarize(&v.accounts, &v.active, v.sync.as_ref(), v.shared.as_ref()));
        if let Ok(mut cache) = cache().lock() {
            if let Some(entry) = cache.get_mut(&base) {
                entry.inflight = false;
                // 한 번 못 읽었다고 알던 계정을 지우지 않는다 — 비면 그 기기 계정이 사라진 것으로 읽힌다.
                if got.is_some() {
                    entry.value = got;
                }
            }
        }
    });
}

pub(crate) fn get(base: &str) -> Option<DeviceAccount> {
    cache().lock().ok()?.get(base)?.value.clone()
}

impl crate::App {
    /// 이 기기의 같은 한 줄. 사용량·신원은 캐시만 읽는다(팝오버를 열 때 한 번).
    pub(crate) fn local_device_account(&self) -> DeviceAccount {
        let active = self.set_claude_account.as_str();
        let notice = crate::session::account_sync_notice(active);
        let rows: Vec<Value> = self
            .set_claude_accounts
            .iter()
            .filter(|account| account.id == active && !active.is_empty())
            .map(|account| {
                let probe = crate::settings::auth_probe(&account.id);
                let usage = self.claude_account_usage(&account.id);
                let logged_in = probe.as_ref().filter(|p| p.verified).map(|p| p.logged_in);
                let stopped = notice.as_ref().filter(|n| n.relogin_slot.as_deref() == Some(active)).and_then(|n| n.code);
                serde_json::json!({
                    "id": account.id,
                    "name": self.claude_account_display(&account.id),
                    "email": probe.as_ref().map(|p| p.email.clone()).unwrap_or_default(),
                    "usage": usage.as_ref().map(|b| b.pct),
                    "usage_label": usage.as_ref().map(|b| b.label.clone()),
                    "relogin": crate::settings::claude_relogin_reason(logged_in, stopped, None),
                })
            })
            .collect();
        let sync = notice.map(|n| serde_json::json!({ "summary": n.summary, "details": n.details, "failed": n.failed }));
        summarize(&rows, active, sync.as_ref(), crate::session::shared_choice_view(active).as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_stalled_follow_wins_over_the_relogin_hint_and_names_the_reason() {
        let rows = [json!({"id":"acct-3","name":"지메일","email":"g@example.test","usage":100.0,"usage_label":"7d","relogin":"account_sync_departing_expired"})];
        let sync = json!({"failed":true,"summary":"다른 기기 계정 따라가기 멈춤","details":"지메일: 지금 계정의 보관 로그인이 만료돼…\n둘째 줄"});
        let shared = json!({"name":"개인사이오닉","follows":false});
        let got = summarize(&rows, "acct-3", Some(&sync), Some(&shared));
        assert_eq!(got.line, "Claude · 지메일 · g@example.test · 7d 100%");
        assert_eq!(got.warning.as_deref(), Some("다른 기기 계정 따라가기 멈춤 · 지메일: 지금 계정의 보관 로그인이 만료돼…"));
        let quiet = summarize(&rows[..], "acct-3", None, Some(&shared));
        assert_eq!(quiet.warning.as_deref(), Some("다시 로그인 필요"));
        let fine = summarize(&[json!({"id":"acct-6","name":"개인","email":"개인"})], "acct-6", None, Some(&json!({"name":"개인","follows":true})));
        assert_eq!(fine, DeviceAccount { line: "Claude · 개인".into(), warning: None });
        let diverged = summarize(&[json!({"id":"acct-6","name":"개인"})], "acct-6", None, Some(&shared));
        assert_eq!(diverged.warning.as_deref(), Some("공통 선택(개인사이오닉)과 다른 계정"));
        assert_eq!(summarize(&[], "", None, None).line, "Claude · 기본 로그인");
    }
}
