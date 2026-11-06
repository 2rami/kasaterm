use super::{Cache, Machine, canonical_roster};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// Rendering cannot reread registration files or wait for a network refresh.
/// Mutations and authorization still use the uncached registration functions.
#[derive(Default, PartialEq, Eq)]
pub struct DisplayIdentity {
    labels: HashMap<String, String>,
    ids: HashMap<String, String>,
}

impl DisplayIdentity {
    fn from_roster(roster: &[Machine], cache: &Cache) -> Self {
        let canonical = canonical_roster(roster.to_vec(), cache);
        let mut snapshot = Self::default();
        for machine in roster {
            let id = machine.machine_id.as_ref()
                .or_else(|| cache.get(&machine.label)?.machine_id.as_ref())
                .filter(|id| !id.is_empty());
            let label = canonical.iter().find(|candidate| {
                candidate.base == machine.base
                    || id.is_some_and(|id| candidate.machine_id.as_ref() == Some(id))
            }).map_or(&machine.label, |candidate| &candidate.label);
            let base = machine.base.trim_end_matches('/').to_string();
            snapshot.labels.insert(base.clone(), label.clone());
            if let Some(id) = id { snapshot.ids.insert(base, id.clone()); }
        }
        snapshot
    }

    pub fn label_for_base(&self, base: &str) -> Option<&str> {
        self.labels.get(base.trim_end_matches('/')).map(String::as_str)
    }

    pub fn same_machine(&self, a: &str, b: &str) -> bool {
        let (a, b) = (a.trim_end_matches('/'), b.trim_end_matches('/'));
        a == b || self.ids.get(a).is_some_and(|id| self.ids.get(b) == Some(id))
    }
}

fn current() -> &'static RwLock<Arc<DisplayIdentity>> {
    static CURRENT: OnceLock<RwLock<Arc<DisplayIdentity>>> = OnceLock::new();
    CURRENT.get_or_init(Default::default)
}

pub(super) fn publish(roster: &[Machine], cache: &Cache) {
    let next = DisplayIdentity::from_roster(roster, cache);
    if let Ok(mut current) = current().write() {
        if **current != next { *current = Arc::new(next); }
    }
}

pub fn display_identity() -> Arc<DisplayIdentity> {
    current().read().map(|snapshot| snapshot.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(label: &str, port: u16, id: Option<&str>, guest: bool) -> Machine {
        let mut machine = super::super::parse(&serde_json::json!([
            {"label": label, "base": format!("http://127.0.0.1:{port}")}
        ])).remove(0);
        machine.machine_id = id.map(str::to_string);
        machine.guest = guest;
        machine
    }

    #[test]
    fn display_snapshot_resolves_aliases_without_reading_registration_again() {
        let roster = vec![machine("chosen", 1001, Some("one"), false),
            machine("announced", 1002, Some("one"), true)];
        let snapshot = DisplayIdentity::from_roster(&roster, &Cache::new());
        drop(roster);
        for _ in 0..1000 {
            assert_eq!(snapshot.label_for_base("http://127.0.0.1:1002/"), Some("chosen"));
            assert!(snapshot.same_machine("http://127.0.0.1:1001", "http://127.0.0.1:1002"));
        }
        assert_eq!(snapshot.label_for_base("http://127.0.0.1:9000"), None);
    }

    #[test]
    fn names_and_localhost_are_not_device_identity() {
        let roster = vec![machine("same-name", 1001, None, false),
            machine("same-name", 1002, None, false)];
        let snapshot = DisplayIdentity::from_roster(&roster, &Cache::new());
        assert!(!snapshot.same_machine(&roster[0].base, &roster[1].base));
        assert!(snapshot.same_machine(&roster[0].base, &format!("{}/", roster[0].base)));
        let empty = DisplayIdentity::from_roster(&[], &Cache::new());
        assert_eq!(empty.label_for_base(&roster[0].base), None);
    }
}
