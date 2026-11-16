//! The order people want their Claude accounts used in, the same on every device of a KASA
//! account (docs/agent-account-selection.md). Slot ids differ per device, so an account is named
//! by its provider account and organization UUIDs, as in the shared choice. Email, labels and
//! secrets never leave the device.

use serde_json::{json, Value};

pub const KEY: &str = "claude_account_order";
const MAX: usize = 32;

/// An account the order names: (account UUID, organization UUID).
pub type Entry = (String, String);

fn entry(v: &Value) -> Option<Entry> {
    let o = v.as_object()?;
    let account = super::choice::id(o.get("account_uuid"))?;
    let organization = super::choice::id(o.get("organization_uuid"))?;
    (o.len() == 2).then(|| (account.to_string(), organization.to_string()))
}

pub fn valid(v: &Value) -> bool {
    let Some(a) = v.as_array() else { return false };
    let entries: Vec<_> = a.iter().map_while(entry).collect();
    a.len() <= MAX
        && entries.len() == a.len()
        && entries.iter().enumerate().all(|(i, e)| !entries[..i].contains(e))
}

/// The order a setting value holds; empty when there is none or it is not one.
pub fn read(v: Option<&Value>) -> Vec<Entry> {
    v.filter(|v| valid(v))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(entry).collect())
        .unwrap_or_default()
}

pub fn value(order: &[Entry]) -> Value {
    Value::Array(order.iter().map(|(a, o)| json!({"account_uuid": a, "organization_uuid": o})).collect())
}

/// The shared order after this device put its accounts in `local` order. Accounts only other
/// devices have keep their places; this device's take the places its accounts held, in the new
/// order, and accounts the order did not name yet go last.
pub fn merge(shared: &[Entry], local: &[Entry]) -> Vec<Entry> {
    let mut mine = local.iter().filter(|e| shared.contains(e));
    let mut out: Vec<Entry> = shared
        .iter()
        .map(|e| if local.contains(e) { mine.next().unwrap_or(e).clone() } else { e.clone() })
        .collect();
    out.extend(local.iter().filter(|e| !shared.contains(e)).cloned());
    out.truncate(MAX);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(n: u8) -> Entry {
        (format!("00000000-0000-0000-0000-0000000000{n:02}"), format!("10000000-0000-0000-0000-0000000000{n:02}"))
    }

    #[test]
    fn only_distinct_uuid_pairs_are_an_order() {
        assert!(valid(&value(&[e(1), e(2)])));
        assert!(valid(&json!([])));
        assert!(!valid(&value(&[e(1), e(1)])), "an account listed twice");
        assert!(!valid(&json!([{"account_uuid": e(1).0, "organization_uuid": e(1).1, "email": "a@b.c"}])), "anything past the UUIDs leaves the device");
        assert!(!valid(&json!([{"account_uuid": "00000000-0000-0000-0000-000000000000", "organization_uuid": e(1).1}])));
        assert!(!valid(&json!({"account_uuid": e(1).0})));
        assert!(!valid(&value(&(1..=33).map(e).collect::<Vec<_>>())));
        assert_eq!(read(Some(&value(&[e(2), e(1)]))), vec![e(2), e(1)]);
        assert!(read(Some(&json!("x"))).is_empty());
    }

    #[test]
    fn a_device_reorders_its_own_accounts_around_the_others() {
        // The home PC alone has e(9); the laptop moves e(3) above e(1).
        let shared = [e(1), e(9), e(3)];
        assert_eq!(merge(&shared, &[e(3), e(1)]), vec![e(3), e(9), e(1)]);
        assert_eq!(merge(&shared, &[e(1), e(3), e(4)]), vec![e(1), e(9), e(3), e(4)], "a new account goes last");
        assert_eq!(merge(&[], &[e(2), e(1)]), vec![e(2), e(1)]);
    }
}
