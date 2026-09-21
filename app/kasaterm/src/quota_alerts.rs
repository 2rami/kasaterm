use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;

fn process_account_dir(line: &str) -> Option<&str> {
    // A successful command-only ps response does not prove a default account.
    let tokens: Vec<_> = line.split_whitespace().collect();
    if !tokens.iter().any(|token| token.starts_with("HOME=") || token.starts_with("PATH=")) {
        return None;
    }
    Some(tokens.iter().find_map(|token| token.strip_prefix("CLAUDE_SECURESTORAGE_CONFIG_DIR=")).unwrap_or(""))
}

pub(crate) fn active_models(backend: Option<&crate::socket::PtyBackend>, active_dir: &str) -> Vec<String> {
    let Some(backend) = backend else { return Vec::new() };
    let cfg = backend.agent_cfg_snapshot();
    let processes = kasa_pty::process_table_shared();
    let mut models = Vec::new();
    for surface in kasa_pty::live_sessions() {
        if kasa_mcp::remote::is_remote_pane(&surface) { continue; }
        let agent = kasa_pty::lookup_session(&surface).and_then(|session| session.shell_pid())
            .and_then(|pid| kasa_pty::agent_pid_for_shell(&processes, pid));
        let Some((kasa_pty::AgentKind::Claude, pid)) = agent else { continue };
        let output = crate::proc::command("ps").args(["eww", "-o", "command=", "-p", &pid.to_string()]).output();
        let Ok(output) = output else { return Vec::new() };
        if !output.status.success() { return Vec::new(); }
        let line = String::from_utf8_lossy(&output.stdout);
        let Some(dir) = process_account_dir(&line) else { return Vec::new() };
        if dir != active_dir { continue; }
        let Some((model, _)) = cfg.get(&surface).filter(|(model, _)| !model.trim().is_empty()) else {
            return Vec::new();
        };
        models.push(model.clone());
    }
    models.sort();
    models.dedup();
    models
}

#[derive(Clone, Debug)]
pub(crate) struct QuotaAlert {
    pub account: String,
    pub label: String,
    pub percent: String,
    pub resets_at: Option<u64>,
}

#[derive(Default)]
pub(crate) struct AlertTracker {
    seen: HashMap<(String, String), (Option<u64>, String)>,
}

impl AlertTracker {
    pub(crate) fn observe(&mut self, account: &str, usage: &Value, fresh: bool) -> Vec<QuotaAlert> {
        if !fresh { return Vec::new(); }
        let windows: Vec<_> = if let Some(limits) = usage.get("limits").and_then(Value::as_array) {
            limits.iter().flat_map(|limit| {
                let identity = serde_json::json!([
                    limit.get("kind"), limit.get("group"), limit.get("scope")
                ]).to_string();
                crate::socket::usage_windows(&serde_json::json!({"limits": [limit]}))
                    .into_iter().map(move |window| (identity.clone(), window))
            }).collect()
        } else {
            crate::socket::usage_windows(usage).into_iter()
                .map(|window| (window.label.clone(), window)).collect()
        };
        let mut alerts = Vec::new();
        for (scope, window) in windows {
            if !window.pct.is_finite() { continue; }
            let key = (account.to_owned(), scope);
            if window.pct < 90.0 {
                self.seen.remove(&key);
                continue;
            }
            // Rust's display rounding is also used by the quota labels.
            let percent = format!("{:.0}", window.pct);
            let state = (window.resets_at, percent.clone());
            if self.seen.get(&key) == Some(&state) { continue; }
            self.seen.insert(key, state);
            alerts.push(QuotaAlert {
                account: account.to_owned(), label: window.label,
                percent, resets_at: window.resets_at,
            });
        }
        alerts
    }
}

#[derive(Default)]
pub(crate) struct FreshUsageCache {
    entries: HashMap<String, (String, Instant, Value)>,
}

impl FreshUsageCache {
    pub(crate) fn clear(&mut self) { self.entries.clear(); }

    pub(crate) fn record(
        &mut self, dir: &str, account: &str,
        response: Option<&(Value, bool, String)>, now: Instant,
    ) {
        self.entries.remove(dir);
        if let Some((usage, false, returned_dir)) = response {
            if returned_dir == dir {
                self.entries.insert(dir.to_owned(), (account.to_owned(), now, usage.clone()));
            }
        }
    }

    pub(crate) fn pressure(
        &self, dir: &str, account: &str, models: &[String], now: Instant,
    ) -> Option<crate::socket::UsagePressure> {
        let (owner, fetched, usage) = self.entries.get(dir)?;
        if owner != account || now.saturating_duration_since(*fetched) > Duration::from_secs(360) {
            return None;
        }
        crate::socket::usage_pressure_for_models(usage, models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_environment_requires_verified_environment_and_keeps_pinned_identity() {
        assert_eq!(process_account_dir("claude"), None);
        assert_eq!(process_account_dir("claude HOME=/fixture PATH=/usr/bin"), Some(""));
        assert_eq!(process_account_dir("claude HOME=/fixture CLAUDE_SECURESTORAGE_CONFIG_DIR=/fixture/account-b"), Some("/fixture/account-b"));
    }

    fn usage(percent: f32, reset: &str) -> Value {
        serde_json::json!({"limits": [{"kind": "weekly_all", "group": "weekly", "percent": percent, "resets_at": reset}]})
    }

    #[test]
    fn threshold_and_display_changes_are_independent() {
        let mut tracker = AlertTracker::default();
        let reset = "2030-01-01T00:00:00Z";
        assert!(tracker.observe("a", &usage(89.6, reset), true).is_empty());
        assert_eq!(tracker.observe("a", &usage(90.0, reset), true).len(), 1);
        assert!(tracker.observe("a", &usage(90.2, reset), true).is_empty());
        assert_eq!(tracker.observe("a", &usage(90.8, reset), true).len(), 1);
        assert!(tracker.observe("a", &usage(90.8, reset), true).is_empty());
    }

    #[test]
    fn stale_reads_do_not_rearm_but_fresh_recovery_and_reset_do() {
        let mut tracker = AlertTracker::default();
        let reset = "2030-01-01T00:00:00Z";
        assert_eq!(tracker.observe("a", &usage(95.0, reset), true).len(), 1);
        assert!(tracker.observe("a", &usage(10.0, reset), false).is_empty());
        assert!(tracker.observe("a", &usage(95.0, reset), true).is_empty());
        assert!(tracker.observe("a", &usage(10.0, reset), true).is_empty());
        assert_eq!(tracker.observe("a", &usage(95.0, reset), true).len(), 1);
        assert_eq!(tracker.observe("a", &usage(95.0, "2030-01-02T00:00:00Z"), true).len(), 1);
    }

    #[test]
    fn shared_model_and_account_scopes_notify_separately() {
        let mut tracker = AlertTracker::default();
        let mut data = usage(91.0, "2030-01-01T00:00:00Z");
        data["limits"].as_array_mut().unwrap().push(serde_json::json!({
            "kind": "weekly_scoped", "group": "weekly", "percent": 91,
            "scope": {"model": {"display_name": "Fable"}}
        }));
        let first = tracker.observe("a", &data, true);
        assert_eq!(first.len(), 2);
        assert_ne!(first[0].label, first[1].label);
        assert!(tracker.observe("a", &data, true).is_empty());
        assert_eq!(tracker.observe("b", &data, true).len(), 2);
    }

    #[test]
    fn candidate_cache_rejects_stale_failed_old_and_remapped_responses() {
        let mut cache = FreshUsageCache::default();
        let now = Instant::now();
        let response = (usage(20.0, "2030-01-01T00:00:00Z"), false, "workbench".into());
        cache.record("workbench", "a", Some(&response), now);
        assert!(cache.pressure("workbench", "a", &[], now).is_some());
        assert!(cache.pressure("workbench", "b", &[], now).is_none());
        assert!(cache.pressure("workbench", "a", &[], now + Duration::from_secs(361)).is_none());
        cache.record("workbench", "a", None, now);
        assert!(cache.pressure("workbench", "a", &[], now).is_none());
        cache.record("workbench", "a", Some(&response), now);
        cache.record("workbench", "a", Some(&(response.0.clone(), true, response.2.clone())), now);
        assert!(cache.pressure("workbench", "a", &[], now).is_none());
        cache.record("workbench", "a", Some(&(response.0, false, "other".into())), now);
        assert!(cache.pressure("workbench", "a", &[], now).is_none());
    }
}
