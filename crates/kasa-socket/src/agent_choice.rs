//! Compare the logins a new session will actually use, without rotating credentials
//! or interpreting missing/stale quota as an empty allowance.
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, PartialEq)]
pub struct Choice {
    pub provider: &'static str,
    pub used_percent: Option<f64>,
    pub reason: &'static str,
}

enum Quota { Known(f64), Unknown, Unavailable }

fn quota(values: &Value, provider: &str) -> Quota {
    let accounts = &values["claude"];
    let (active_key, rows_key) = if provider == "codex" { ("codex_account", "codex_accounts") } else { ("account", "accounts") };
    let active = accounts[active_key].as_str().unwrap_or("");
    let Some(row) = accounts[rows_key].as_array().and_then(|rows|
        rows.iter().find(|row| row["id"].as_str() == Some(active))) else { return Quota::Unknown };
    if row["logged_in"].as_bool() == Some(false) { return Quota::Unavailable; }
    if row["logged_in"].as_bool() != Some(true) || row["usage_stale"].as_bool() != Some(false)
        || row["usage_state"].as_str() != Some("ready") { return Quota::Unknown; }
    match row["usage"].as_f64().filter(|p| p.is_finite() && (0.0..=100.0).contains(p)) {
        Some(p) if p >= 100.0 => Quota::Unavailable,
        Some(p) => Quota::Known(p),
        None => Quota::Unknown,
    }
}

pub fn choose(values: &Value) -> Result<Choice, &'static str> {
    let preferred = if values["agents"]["preferred_agent"] == "codex" { "codex" } else { "claude" };
    let providers = if preferred == "claude" { ["claude", "codex"] } else { ["codex", "claude"] };
    let mut best: Option<Choice> = None;
    let mut unknown = None;
    for provider in providers {
        match quota(values, provider) {
            Quota::Known(used) if best.as_ref().is_none_or(|v| used < v.used_percent.unwrap()) => {
                best = Some(Choice { provider, used_percent: Some(used), reason: "remaining_usage" });
            }
            Quota::Unknown if unknown.is_none() => { unknown = Some(provider); }
            _ => {}
        }
    }
    best.or_else(|| unknown.map(|provider| Choice { provider, used_percent: None, reason: "usage_unavailable" }))
        .ok_or("Claude·Codex의 현재 로그인 계정이 모두 로그아웃됐거나 한도에 도달했어요")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(claude: f64, codex: f64) -> Value {
        json!({"agents":{"preferred_agent":"claude"},"claude":{"account":"c","codex_account":"",
            "accounts":[{"id":"c","logged_in":true,"usage":claude,"usage_stale":false,"usage_state":"ready"}],
            "codex_accounts":[{"id":"","logged_in":true,"usage":codex,"usage_stale":false,"usage_state":"ready"}]}})
    }
    #[test]
    fn compares_current_accounts_and_never_treats_a_standby_as_the_live_login() {
        let mut v = values(90.0, 30.0);
        v["claude"]["accounts"].as_array_mut().unwrap().push(json!({"id":"standby","logged_in":true,"usage":0,"usage_stale":false,"usage_state":"ready"}));
        assert_eq!(choose(&v).unwrap().provider, "codex");
        assert_eq!(choose(&values(10.0, 80.0)).unwrap().provider, "claude");
    }
    #[test]
    fn stale_and_missing_measurements_are_not_free_capacity() {
        let mut v = values(0.0, 76.0);
        v["claude"]["accounts"][0]["usage_stale"] = json!(true);
        assert_eq!(choose(&v).unwrap().provider, "codex");
        v["claude"]["codex_accounts"][0]["usage_state"] = json!("failed");
        let fallback = choose(&v).unwrap();
        assert_eq!(fallback.provider, "claude");
        assert_eq!(fallback.used_percent, None);
        assert_eq!(fallback.reason, "usage_unavailable");
    }
    #[test]
    fn login_failures_and_full_allowances_are_excluded_and_ties_keep_preference() {
        let mut v = values(10.0, 10.0);
        v["agents"]["preferred_agent"] = json!("codex");
        assert_eq!(choose(&v).unwrap().provider, "codex");
        v["claude"]["codex_accounts"][0]["logged_in"] = json!(false);
        assert_eq!(choose(&v).unwrap().provider, "claude");
        v["claude"]["accounts"][0]["usage"] = json!(100.0);
        assert!(choose(&v).is_err());
    }
}
