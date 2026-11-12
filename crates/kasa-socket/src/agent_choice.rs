//! New sessions compare all usable slots; legacy provider-only callers keep the current-login comparison.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, PartialEq)]
pub struct Choice {
    pub provider: &'static str,
    pub used_percent: Option<f64>,
    pub reason: &'static str,
    pub account_id: String,
    pub account_label: String,
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

pub fn choose_current(values: &Value) -> Result<Choice, &'static str> {
    let preferred = if values["agents"]["preferred_agent"] == "codex" { "codex" } else { "claude" };
    let providers = if preferred == "claude" { ["claude", "codex"] } else { ["codex", "claude"] };
    let mut best: Option<Choice> = None;
    let mut unknown = None;
    for provider in providers {
        match quota(values, provider) {
            Quota::Known(used) if best.as_ref().is_none_or(|v| used < v.used_percent.unwrap()) => {
                best = Some(Choice { provider, used_percent: Some(used), reason: "remaining_usage", account_id: active_id(values,provider).into(), account_label: String::new() });
            }
            Quota::Unknown if unknown.is_none() => { unknown = Some(provider); }
            _ => {}
        }
    }
    best.or_else(|| unknown.map(|provider| Choice { provider, used_percent: None, reason: "usage_unavailable", account_id: active_id(values,provider).into(), account_label: String::new() }))
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
    fn row(id:&str, used:f64)->Value {
        json!({"id":id,"label":id,"logged_in":true,"usage":used,"usage_stale":false,"usage_state":"ready"})
    }

    #[test]
    fn all_slots_choose_the_actual_standby_account() {
        let mut v=values(47.0,17.0);
        v["claude"]["accounts"].as_array_mut().unwrap().push(row("gmail",0.0));
        let choice=choose(&v).unwrap();
        assert_eq!((choice.provider,choice.account_id.as_str(),choice.used_percent),("claude","gmail",Some(0.0)));
        v["claude"]["accounts"][1]["usage"]=json!(40.0);
        v["claude"]["codex_accounts"].as_array_mut().unwrap().push(row("codex-spare",2.0));
        let choice=choose(&v).unwrap();
        assert_eq!((choice.provider,choice.account_id.as_str()),("codex","codex-spare"));
        assert_eq!(v["claude"]["account"],"c");
    }

    #[test]
    fn unknown_or_revoked_slots_are_not_zero_and_selection_is_rechecked() {
        let mut v=values(47.0,17.0);
        v["claude"]["accounts"].as_array_mut().unwrap().extend([row("stale",0.0),row("revoked",0.0),row("full",100.0)]);
        v["claude"]["accounts"][1]["usage_stale"]=json!(true);
        v["claude"]["accounts"][2]["usage_state"]=json!("logged_out");
        assert_eq!(choose(&v).unwrap().provider,"codex");
        let selected=Selection{provider:"claude".into(),account_id:"stale".into()};
        assert!(validate_selection(&v,&selected).is_err());
        assert!(validate_resume(&v,&selected).is_ok());
        v["claude"]["accounts"][1]["logged_in"]=json!(false);
        assert!(validate_selection(&v,&selected).is_err());
        assert!(Selection{provider:"claude".into(),account_id:"../other".into()}.validate().is_err());
    }

    #[test]
    fn resuming_registered_work_does_not_require_a_warm_usage_cache() {
        let mut v=values(100.0,17.0);
        let account=Selection {provider:"claude".into(),account_id:"c".into()};
        assert!(validate_selection(&v,&account).is_err());
        assert!(validate_resume(&v,&account).is_ok());
        v["claude"]["accounts"][0]["logged_in"]=Value::Null;
        v["claude"]["accounts"][0]["usage_state"]=json!("loading");
        assert!(validate_resume(&v,&account).is_ok());
        assert!(validate_selection(&v,&account).is_err());
        v["claude"]["accounts"][0]["usage_state"]=json!("logged_out");
        assert!(validate_resume(&v,&account).is_err());
    }

    #[test]
    fn ties_keep_provider_and_current_slot_and_unknown_fallback_never_picks_a_standby() {
        let mut v=values(5.0,5.0);
        v["claude"]["accounts"].as_array_mut().unwrap().push(row("aaa",5.0));
        assert_eq!(choose(&v).unwrap().account_id,"c");
        v["agents"]["preferred_agent"]=json!("codex");
        assert_eq!(choose(&v).unwrap().provider,"codex");
        for key in ["accounts","codex_accounts"] {
            for row in v["claude"][key].as_array_mut().unwrap() {row["usage_stale"]=json!(true);}
        }
        let choice=choose(&v).unwrap();
        assert_eq!((choice.provider,choice.account_id.as_str(),choice.used_percent),("codex","",None));
    }

    #[test]
    fn compares_current_accounts_and_never_treats_a_standby_as_the_live_login() {
        let mut v = values(90.0, 30.0);
        v["claude"]["accounts"].as_array_mut().unwrap().push(json!({"id":"standby","logged_in":true,"usage":0,"usage_stale":false,"usage_state":"ready"}));
        assert_eq!(choose_current(&v).unwrap().provider, "codex");
        assert_eq!(choose_current(&values(10.0, 80.0)).unwrap().provider, "claude");
    }
    #[test]
    fn stale_and_missing_measurements_are_not_free_capacity() {
        let mut v = values(0.0, 76.0);
        v["claude"]["accounts"][0]["usage_stale"] = json!(true);
        assert_eq!(choose_current(&v).unwrap().provider, "codex");
        v["claude"]["codex_accounts"][0]["usage_state"] = json!("failed");
        let fallback = choose_current(&v).unwrap();
        assert_eq!(fallback.provider, "claude");
        assert_eq!(fallback.used_percent, None);
        assert_eq!(fallback.reason, "usage_unavailable");
    }
    #[test]
    fn login_failures_and_full_allowances_are_excluded_and_ties_keep_preference() {
        let mut v = values(10.0, 10.0);
        v["agents"]["preferred_agent"] = json!("codex");
        assert_eq!(choose_current(&v).unwrap().provider, "codex");
        v["claude"]["codex_accounts"][0]["logged_in"] = json!(false);
        assert_eq!(choose_current(&v).unwrap().provider, "claude");
        v["claude"]["accounts"][0]["usage"] = json!(100.0);
        assert!(choose_current(&v).is_err());
    }
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub provider: String,
    pub account_id: String,
}

impl Selection {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.provider.as_str(), "claude" | "codex") { return Err("unknown account provider"); }
        if self.account_id.len() > 128 || !self.account_id.bytes().all(|c|c.is_ascii_alphanumeric() || c==b'-' || c==b'_') {
            return Err("invalid account id");
        }
        if self.provider == "claude" && self.account_id.is_empty() { return Err("Claude account id is required"); }
        Ok(())
    }

    pub fn command(&self, command: &str) -> Result<String, &'static str> {
        self.validate()?;
        let account=if self.account_id.is_empty() {"@default"} else {&self.account_id};
        Ok(format!("kasaterm-cli account-run {} '{account}' -- {command}",self.provider))
    }
}

pub fn selection(value: &Value) -> Result<Selection, &'static str> {
    let selected = Selection {provider:value["provider"].as_str().ok_or("missing provider")?.into(),
        account_id:value["account_id"].as_str().ok_or("account-aware launch needs an updated host")?.into()};
    selected.validate()?;
    Ok(selected)
}

fn active_id<'a>(values: &'a Value, provider: &str) -> &'a str {
    values["claude"][if provider=="claude" {"account"} else {"codex_account"}].as_str().unwrap_or("")
}

fn rows<'a>(values: &'a Value, provider: &str) -> impl Iterator<Item=&'a Value> {
    values["claude"][if provider=="claude" {"accounts"} else {"codex_accounts"}].as_array().into_iter().flatten()
}

fn registered_row<'a>(values: &'a Value, selected: &Selection) -> Result<&'a Value, &'static str> {
    selected.validate()?;
    rows(values,&selected.provider).find(|r|r["id"].as_str()==Some(selected.account_id.as_str())).ok_or("selected account no longer exists")
}

fn account_row<'a>(values: &'a Value, selected: &Selection) -> Result<&'a Value, &'static str> {
    let row=registered_row(values,selected)?;
    if row["logged_in"]!=true || row["usage_state"]=="logged_out" {return Err("selected account login is not verified");}
    Ok(row)
}

pub fn validate_resume(values: &Value, selected: &Selection) -> Result<(), &'static str> {
    let row=registered_row(values,selected)?;
    // The startup UI cache may not be populated yet; resume preserves a registered slot, not a quota-based choice.
    if row["logged_in"]==false || row["usage_state"]=="logged_out" {return Err("saved account is logged out");}
    Ok(())
}

pub fn validate_selection(values: &Value, selected: &Selection) -> Result<(), &'static str> {
    let row=account_row(values,selected)?;
    let fresh=row["usage_state"]=="ready" && row["usage_stale"]==false;
    if fresh && row["usage"].as_f64().is_some_and(|p|p>=100.0) {
        return Err("selected account reached its limit; choose again");
    }
    if selected.account_id!=active_id(values,&selected.provider) && (!fresh || row["usage"].as_f64().is_none_or(|p|!p.is_finite() || p<0.0)) {
        return Err("selected standby account usage is no longer fresh; choose again");
    }
    Ok(())
}

pub fn choose(values: &Value) -> Result<Choice, &'static str> {
    let preferred = if values["agents"]["preferred_agent"] == "codex" {"codex"} else {"claude"};
    let providers = if preferred=="claude" {["claude","codex"]} else {["codex","claude"]};
    let mut known = Vec::new();
    let mut fallback = None;
    for (rank,provider) in providers.into_iter().enumerate() {
        for row in rows(values,provider) {
            let Some(id) = row["id"].as_str() else { continue };
            let selected = Selection {provider:provider.into(),account_id:id.into()};
            if validate_selection(values,&selected).is_err() { continue; }
            let current = id==active_id(values,provider);
            let label = row["label"].as_str().filter(|s|!s.is_empty()).or_else(||row["name"].as_str()).unwrap_or(id).to_string();
            let used = (row["usage_state"]=="ready" && row["usage_stale"]==false)
                .then(||row["usage"].as_f64()).flatten().filter(|p|p.is_finite() && (0.0..100.0).contains(p));
            let choice = Choice {provider,account_id:id.into(),account_label:label,used_percent:used,
                reason:if used.is_some() {"remaining_usage"} else {"usage_unavailable"}};
            if let Some(used) = used { known.push((used,rank,!current,choice)); }
            else if current && fallback.is_none() { fallback=Some(choice); }
        }
    }
    known.sort_by(|a,b|a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.account_id.cmp(&b.3.account_id)));
    known.into_iter().next().map(|(_,_,_,choice)|choice).or(fallback)
        .ok_or("사용 가능한 Claude·Codex 계정을 확인하지 못했어요. 로그인과 사용량을 확인해 주세요")
}
