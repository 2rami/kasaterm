//! The Claude account every device of a KASA account should use (docs/agent-account-selection.md).
//!
//! Only the provider's account and organization UUIDs carry authority; email and organization
//! names are hints for display and for finding the gateway chain. Slot ids, labels and secrets
//! never leave the device.
//!
//! Order comes from the relay's revision, never from a device clock. A device writes an intent
//! without `rev`, bound to the snapshot revision it had seen (`base`) and to every intent of its own
//! that the sync has actually sent and not yet seen answered (`after`) — any of those may already sit
//! on the relay, however many choices came after it. Unsent intents can never be there and are not
//! listed, so the list grows with unanswered sends, not with clicks. The relay stamps
//! `rev` when it commits and refuses an intent that did not see the choice it would replace,
//! so a device returning from a long offline stretch cannot undo a later choice.

use serde_json::{Map, Value};

pub const KEY: &str = "claude_account_choice";
/// Local-only: the KASA account (`local::account_key`) the value above belongs to. Never synced, so a choice
/// made under one KASA sign-in is not sent or followed under another.
pub const OWNER_KEY: &str = "claude_account_choice_owner";
/// Local-only: intents this device sent that the relay has not been seen to answer, newest first.
pub const SENT_KEY: &str = "claude_account_choice_sent";
const CHAIN: usize = 8;

fn text(v: &Value, max: usize) -> bool {
    v.as_str().is_some_and(|s| s.chars().count() <= max && !s.chars().any(char::is_control))
}

fn id(v: Option<&Value>) -> Option<uuid::Uuid> {
    let raw = v?.as_str()?;
    let parsed = uuid::Uuid::parse_str(raw).ok()?;
    (raw.len() == 36 && !parsed.is_nil()).then_some(parsed)
}

pub fn valid(v: &Value) -> bool {
    let Some(o) = v.as_object() else { return false };
    o.len() <= 8
        && o.iter().all(|(key, value)| match key.as_str() {
            "account_uuid" | "organization_uuid" | "intent" => id(Some(value)).is_some(),
            "after" => value.as_array().is_some_and(|a| !a.is_empty() && a.len() <= CHAIN && a.iter().all(|v| id(Some(v)).is_some())),
            "email" => text(value, 320),
            "org" => text(value, 1024),
            "base" => value.as_u64().is_some(),
            "rev" => value.as_u64().is_some_and(|rev| rev > 0),
            _ => false,
        })
        && ["account_uuid", "organization_uuid", "intent", "base"].iter().all(|key| o.contains_key(*key))
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub fn intent(v: &Value) -> Option<&str> {
    field(v, "intent")
}

pub fn rev(v: &Value) -> Option<u64> {
    v.get("rev").and_then(Value::as_u64)
}

pub fn base(v: &Value) -> u64 {
    v.get("base").and_then(Value::as_u64).unwrap_or(0)
}

/// A local value without `rev` is this device's intent that the relay has not answered yet.
pub fn pending(v: &Value) -> bool {
    rev(v).is_none()
}

/// Same provider account and organization — the only identity comparison that authorizes anything.
pub fn same_identity(a: &Value, b: &Value) -> bool {
    match (id(a.get("account_uuid")), id(a.get("organization_uuid")), id(b.get("account_uuid")), id(b.get("organization_uuid"))) {
        (Some(a1), Some(a2), Some(b1), Some(b2)) => a1 == b1 && a2 == b2,
        _ => false,
    }
}

/// Builds a new intent. `seen` is the snapshot revision this device last observed, `current` the local value
/// right now and `sent` this device's sent-but-unanswered intents, so a choice made while any of those is in
/// flight still replaces it once it lands.
pub fn new_intent(
    account_uuid: &str,
    organization_uuid: &str,
    email: &str,
    org: &str,
    seen: u64,
    current: Option<&Value>,
    sent: &[String],
) -> Option<Value> {
    let mut o = Map::new();
    o.insert("account_uuid".into(), Value::String(account_uuid.to_lowercase()));
    o.insert("organization_uuid".into(), Value::String(organization_uuid.to_lowercase()));
    let (email, org) = (email.trim(), org.trim());
    if !email.is_empty() { o.insert("email".into(), Value::String(email.into())); }
    if !org.is_empty() { o.insert("org".into(), Value::String(org.into())); }
    o.insert("intent".into(), Value::String(uuid::Uuid::new_v4().to_string()));
    let mut base = seen;
    if let Some(current) = current.filter(|v| valid(v)) {
        match rev(current) {
            Some(rev) => base = base.max(rev),
            None => base = base.max(self::base(current)),
        }
    }
    let chain: Vec<Value> = sent.iter().take(CHAIN).map(|id| Value::String(id.clone())).collect();
    if !chain.is_empty() {
        o.insert("after".into(), Value::Array(chain));
    }
    o.insert("base".into(), Value::from(base));
    let value = Value::Object(o);
    valid(&value).then_some(value)
}

/// Whether a pending intent may replace `remote`: it saw that choice, or it chains to the
/// intent that produced it.
pub fn may_replace(pending: &Value, remote: Option<&Value>) -> bool {
    let Some(remote) = remote else { return true };
    if rev(remote).is_none_or(|rev| rev <= base(pending)) {
        return true;
    }
    let answered = intent(remote);
    pending.get("after").and_then(Value::as_array).is_some_and(|after| after.iter().any(|v| v.as_str() == answered))
}

pub fn sent(settings: &Value) -> Vec<String> {
    settings.get(SENT_KEY).and_then(Value::as_array).into_iter().flatten()
        .filter(|v| id(Some(v)).is_some()).filter_map(|v| v.as_str().map(str::to_string)).take(CHAIN).collect()
}

/// Records that the sync is about to send `intent`. The list keeps the newest sends; one send happens per pass, so
/// the cap only drops sends that went unanswered for that many passes.
pub fn mark_sent(settings: &mut Value, intent: &str) {
    let mut list = sent(settings);
    list.retain(|id| id != intent);
    list.insert(0, intent.into());
    list.truncate(CHAIN);
    settings[SENT_KEY] = Value::from(list);
}

/// Once the relay shows one of our sends, it and every older send are settled: an older intent can no longer
/// replace the newer one that landed.
pub fn prune_sent(settings: &mut Value, remote: Option<&Value>) {
    let mut list = sent(settings);
    if let Some(at) = remote.and_then(intent).and_then(|landed| list.iter().position(|id| id == landed)) {
        list.truncate(at);
    }
    match settings.as_object_mut() {
        Some(o) if list.is_empty() => { o.remove(SENT_KEY); }
        Some(_) => settings[SENT_KEY] = Value::from(list),
        None => {}
    }
}

/// The choice in these local settings if it belongs to the signed-in KASA account.
pub fn owned<'a>(settings: &'a Value, account: &str) -> Option<&'a Value> {
    (settings.get(OWNER_KEY).and_then(Value::as_str) == Some(account)).then(|| settings.get(KEY)).flatten().filter(|v| valid(v))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing differs.
    Keep,
    /// Send this device's intent.
    Publish(Value),
    /// The local value must become the relay's canonical one — an answered intent, a later choice
    /// from another device, or a stale intent that the relay would refuse.
    Adopt(Option<Value>),
}

/// What one sync pass does with the choice. It is not part of the generic field delta: a remote
/// A→B→A that leaves the previous value unchanged must still overwrite a refused local intent.
pub fn plan(local: Option<&Value>, remote: Option<&Value>) -> Plan {
    let local = local.filter(|v| valid(v));
    let remote = remote.filter(|v| valid(v) && rev(v).is_some());
    match local {
        Some(mine) if pending(mine) => {
            if remote.is_some_and(|remote| intent(remote) == intent(mine)) {
                return Plan::Adopt(remote.cloned());
            }
            if may_replace(mine, remote) { Plan::Publish(mine.clone()) } else { Plan::Adopt(remote.cloned()) }
        }
        _ if local == remote => Plan::Keep,
        // No client deletes a choice: a relay without one (older, or not yet chosen) leaves the local value alone.
        Some(_) if remote.is_none() => Plan::Keep,
        _ => Plan::Adopt(remote.cloned()),
    }
}

/// The relay's half: refuse an intent that did not see the choice it would replace, keep a
/// retried intent idempotent, and stamp the revision it commits at.
#[derive(Debug, PartialEq, Eq)]
pub enum Commit {
    Stamp(Value),
    Unchanged(Value),
    Stale,
    Invalid,
}

pub fn commit(current: Option<&Value>, proposed: &Value, current_revision: u64) -> Commit {
    if !valid(proposed) || !pending(proposed) || base(proposed) > current_revision {
        return Commit::Invalid;
    }
    if let Some(current) = current.filter(|v| valid(v)) {
        if intent(current) == intent(proposed) {
            return Commit::Unchanged(current.clone());
        }
        if !may_replace(proposed, Some(current)) {
            return Commit::Stale;
        }
    }
    let mut stamped = proposed.clone();
    stamped["rev"] = Value::from(current_revision + 1);
    Commit::Stamp(stamped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "00000000-0000-4000-8000-00000000000a";
    const B: &str = "00000000-0000-4000-8000-00000000000b";
    const ORG: &str = "00000000-0000-4000-8000-0000000000f0";

    fn stamped(account: &str, seen: u64, rev: u64) -> Value {
        let mut v = new_intent(account, ORG, "person@example.test", "Org", seen, None, &[]).unwrap();
        v["rev"] = json!(rev);
        v
    }

    #[test]
    fn values_carry_no_slot_label_or_secret_and_only_uuids_identify() {
        let v = new_intent(A, ORG, " person@example.test ", "Org", 3, None, &[]).unwrap();
        assert_eq!(v["base"], 3);
        assert!(v.get("rev").is_none() && pending(&v));
        for key in ["slot", "label", "token", "accessToken"] {
            let mut bad = v.clone();
            bad[key] = json!("x");
            assert!(!valid(&bad), "{key}");
        }
        let mut renamed = v.clone();
        renamed["email"] = json!("other@example.test");
        renamed["org"] = json!("Renamed");
        assert!(same_identity(&v, &renamed));
        let mut twin = v.clone();
        twin["account_uuid"] = json!(B);
        assert!(!same_identity(&v, &twin), "same names, different provider account");
        assert!(!valid(&json!({"account_uuid":uuid::Uuid::nil().to_string(),"organization_uuid":ORG,"intent":A,"base":0})));
    }

    #[test]
    fn an_intent_that_missed_a_later_choice_is_refused_and_the_remote_wins() {
        let remote = stamped(B, 10, 11);
        let offline = new_intent(A, ORG, "", "", 10, None, &[]).unwrap();
        assert_eq!(plan(Some(&offline), Some(&remote)), Plan::Adopt(Some(remote.clone())));
        assert_eq!(commit(Some(&remote), &offline, 11), Commit::Stale);
        let informed = new_intent(A, ORG, "", "", 11, None, &[]).unwrap();
        assert!(matches!(plan(Some(&informed), Some(&remote)), Plan::Publish(_)));
        let Commit::Stamp(done) = commit(Some(&remote), &informed, 11) else { panic!() };
        assert_eq!(done["rev"], 12);
    }

    #[test]
    fn remote_a_b_a_still_replaces_a_refused_local_intent() {
        let a_again = stamped(A, 12, 13);
        let stale = new_intent(B, ORG, "", "", 10, None, &[]).unwrap();
        assert_eq!(plan(Some(&stale), Some(&a_again)), Plan::Adopt(Some(a_again.clone())));
        let old_synced = stamped(A, 10, 11);
        assert_eq!(plan(Some(&old_synced), Some(&a_again)), Plan::Adopt(Some(a_again)));
    }

    fn choice_id(v: &Value) -> String {
        intent(v).unwrap().to_string()
    }

    #[test]
    fn a_new_choice_chains_to_its_own_sent_intent_but_not_to_someone_elses() {
        let mut local = json!({});
        let first = new_intent(A, ORG, "", "", 10, None, &[]).unwrap();
        mark_sent(&mut local, intent(&first).unwrap());
        let second = new_intent(B, ORG, "", "", 10, Some(&first), &sent(&local)).unwrap();
        assert_eq!(second["after"], json!([first["intent"]]));
        let Commit::Stamp(first_done) = commit(None, &first, 10) else { panic!() };
        assert!(matches!(plan(Some(&second), Some(&first_done)), Plan::Publish(_)));
        assert!(matches!(commit(Some(&first_done), &second, 11), Commit::Stamp(_)));
        let foreign = stamped(A, 10, 11);
        assert_eq!(commit(Some(&foreign), &second, 11), Commit::Stale);
        let again = new_intent(A, ORG, "", "", 10, Some(&first), &sent(&local)).unwrap();
        assert_ne!(again["intent"], first["intent"], "re-choosing the same account is a new intent");
    }

    #[test]
    fn any_number_of_choices_after_a_send_keep_the_last_once_the_send_lands() {
        let mut local = json!({});
        let first = new_intent(A, ORG, "", "", 10, None, &[]).unwrap();
        mark_sent(&mut local, intent(&first).unwrap());
        let mut last = first.clone();
        for n in 0..20 {
            let account = if n % 2 == 0 { B } else { A };
            last = new_intent(account, ORG, "", "", 10, Some(&last), &sent(&local)).unwrap();
        }
        assert_eq!(last["after"], json!([first["intent"]]), "unsent clicks grew the chain or cut the send in flight");
        let Commit::Stamp(first_done) = commit(None, &first, 10) else { panic!() };
        assert!(matches!(plan(Some(&last), Some(&first_done)), Plan::Publish(_)));
        let Commit::Stamp(done) = commit(Some(&first_done), &last, 11) else { panic!("the last choice was refused") };
        assert_eq!(done["rev"], 12);

        // Many unanswered passes: the newest sends stay and the send in flight is always first.
        for _ in 0..CHAIN + 3 {
            let next = new_intent(B, ORG, "", "", 10, Some(&last), &sent(&local)).unwrap();
            mark_sent(&mut local, intent(&next).unwrap());
            last = next;
        }
        assert_eq!(sent(&local).first(), Some(&choice_id(&last)));
        assert_eq!(sent(&local).len(), CHAIN);
        let after_flight = new_intent(A, ORG, "", "", 10, Some(&last), &sent(&local)).unwrap();
        let Commit::Stamp(flight_done) = commit(None, &last, 10) else { panic!() };
        assert!(matches!(commit(Some(&flight_done), &after_flight, 11), Commit::Stamp(_)));
        prune_sent(&mut local, Some(&flight_done));
        assert!(sent(&local).is_empty() && local.get(SENT_KEY).is_none());
    }

    #[test]
    fn a_retried_or_late_answer_consumes_only_its_own_intent() {
        let mine = new_intent(A, ORG, "", "", 4, None, &[]).unwrap();
        let Commit::Stamp(done) = commit(None, &mine, 4) else { panic!() };
        assert_eq!(commit(Some(&done), &mine, 5), Commit::Unchanged(done.clone()));
        assert_eq!(plan(Some(&mine), Some(&done)), Plan::Adopt(Some(done.clone())));
        let newer_same_account = new_intent(A, ORG, "", "", 4, Some(&mine), &[choice_id(&mine)]).unwrap();
        assert!(matches!(plan(Some(&newer_same_account), Some(&done)), Plan::Publish(_)));
    }

    #[test]
    fn revisions_the_relay_never_had_are_refused() {
        let ahead = new_intent(A, ORG, "", "", 500, None, &[]).unwrap();
        assert_eq!(commit(None, &ahead, 20), Commit::Invalid);
    }
}
