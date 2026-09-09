//! Stable surface identity, independent of reusable pane numbers and agent sessions.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashMap<String, String>> {
    static KEYS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn ensure(id: &str) -> String {
    registry().lock().unwrap_or_else(|e| e.into_inner())
        .entry(id.to_owned()).or_insert_with(|| uuid::Uuid::new_v4().to_string()).clone()
}

pub fn set(id: &str, key: &str) {
    registry().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), key.to_owned());
}

pub fn get(id: &str) -> Option<String> {
    registry().lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

pub fn remove(id: &str) {
    registry().lock().unwrap_or_else(|e| e.into_inner()).remove(id);
}

fn text<'a>(record: &'a Value, field: &str) -> Option<&'a str> {
    record.get(field)?.as_str().filter(|s| !s.trim().is_empty())
}

// Records occur under leaf (including undocked leaves), a leaf's tabs, and
// legacy stashed_panes[].rec. The stashed wrapper is not itself a surface.
// Do not interpret arbitrary nested metadata containing `pane_id` as a surface.
fn walk(value: &mut Value, visit: &mut impl FnMut(&mut Value)) {
    match value {
        Value::Object(object) => {
            for (name, child) in object {
                if name == "leaf" {
                    visit(child);
                } else if name == "tabs" {
                    if let Some(tabs) = child.as_array_mut() {
                        for tab in tabs { visit(tab); }
                    }
                } else if name == "stashed_panes" {
                    if let Some(stashed) = child.as_array_mut() {
                        for entry in stashed {
                            if let Some(rec) = entry.get_mut("rec") { visit(rec); }
                        }
                    }
                }
                walk(child, visit);
            }
        }
        Value::Array(values) => {
            for child in values { walk(child, visit); }
        }
        _ => {}
    }
}

type SessionIndex = HashMap<String, HashSet<(String, String)>>;

fn walk_read(value: &Value, visit: &mut impl FnMut(&Value)) {
    match value {
        Value::Object(object) => {
            for (name, child) in object {
                if name == "leaf" {
                    visit(child);
                } else if name == "tabs" {
                    if let Some(tabs) = child.as_array() {
                        for tab in tabs { visit(tab); }
                    }
                } else if name == "stashed_panes" {
                    if let Some(stashed) = child.as_array() {
                        for entry in stashed {
                            if let Some(rec) = entry.get("rec") { visit(rec); }
                        }
                    }
                }
                walk_read(child, visit);
            }
        }
        Value::Array(values) => {
            for child in values { walk_read(child, visit); }
        }
        _ => {}
    }
}

fn index(state: &Value) -> SessionIndex {
    let mut result = SessionIndex::new();
    walk_read(state, &mut |rec| {
        if let (Some(sid), Some(id)) = (text(rec, "session_id"), text(rec, "pane_id")) {
            let key = text(rec, "surface_key").map(str::to_owned)
                .unwrap_or_else(|| format!("legacy:{id}"));
            result.entry(sid.to_owned()).or_default().insert((id.to_owned(), key));
        }
    });
    result
}

/// Stamp old snapshots before restoring their panes. This is a pure transform:
/// it does not touch the live registry, disk, network, or original JSON value.
/// An exact, unambiguous session match is used only to migrate old numbering to
/// a stable key once; an existing surface key always survives agent replacement.
pub fn prepare_restore_state(state: &Value, previous: Option<&Value>) -> Value {
    let previous = previous.map(index).unwrap_or_default();
    let current = index(state);
    let mut prepared = state.clone();
    walk(&mut prepared, &mut |rec| {
        if text(rec, "surface_key").is_some() { return; }
        let Some(id) = text(rec, "pane_id") else { return };
        let inherited = text(rec, "session_id")
            .filter(|sid| current.get(*sid).is_some_and(|rows| rows.len() == 1))
            .and_then(|sid| previous.get(sid))
            .filter(|rows| rows.len() == 1)
            .and_then(|rows| rows.iter().next())
            .map(|(_, key)| key.clone());
        let key = inherited.unwrap_or_else(|| format!("legacy:{id}"));
        rec["surface_key"] = Value::String(key);
    });
    prepared
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_keeps_key_until_explicit_replacement_or_removal() {
        let id = format!("test-{}", uuid::Uuid::new_v4());
        assert_eq!(get(&id), None);
        let first = ensure(&id);
        assert_eq!(uuid::Uuid::parse_str(&first).unwrap().get_version_num(), 4);
        assert_eq!(ensure(&id), first);
        set(&id, "legacy:%4");
        assert_eq!(ensure(&id), "legacy:%4");
        remove(&id);
        assert_eq!(get(&id), None);
    }

    #[test]
    fn migrated_numbers_follow_unique_previous_record_across_all_shapes() {
        let previous = json!({"sessions":[{"windows":[{"split":{
            "a":{"leaf":{"pane_id":"%4","session_id":"a","tabs":[{"pane_id":"%8","session_id":"b"}]}},
            "b":{"leaf":{"pane_id":"%7","session_id":"c","surface_key":"stable-c"}}
        }}],"undocked":[{"leaf":{"pane_id":"%2","session_id":"d"}}]}]});
        let current = json!({"sessions":[{"windows":[{"split":{
            "a":{"leaf":{"pane_id":"%3","session_id":"a","tabs":[{"pane_id":"%2","session_id":"b"}]}},
            "b":{"leaf":{"pane_id":"%1","session_id":"c"}}
        }}],"undocked":[{"leaf":{"pane_id":"%0","session_id":"d"}}]}]});
        let prepared = prepare_restore_state(&current, Some(&previous));
        let session = &prepared["sessions"][0];
        let split = &session["windows"][0]["split"];
        assert_eq!(split["a"]["leaf"]["surface_key"], "legacy:%4");
        assert_eq!(split["a"]["leaf"]["tabs"][0]["surface_key"], "legacy:%8");
        assert_eq!(split["b"]["leaf"]["surface_key"], "stable-c");
        assert_eq!(session["undocked"][0]["leaf"]["surface_key"], "legacy:%2");
        assert!(current["sessions"][0]["windows"][0]["split"]["a"]["leaf"].get("surface_key").is_none());
        assert_eq!(prepare_restore_state(&prepared, None), prepared);
    }

    #[test]
    fn existing_key_survives_replaced_agent_session() {
        let previous = json!({"leaf":{"pane_id":"%4","session_id":"old","surface_key":"original"}});
        let current = json!({"leaf":{"pane_id":"%3","session_id":"new","surface_key":"original"}});
        assert_eq!(prepare_restore_state(&current, Some(&previous)), current);
    }

    #[test]
    fn legacy_stashed_records_inherit_keys_and_keep_tabs() {
        let previous = json!({"stashed_panes":[
            {"pane_id":"%4","rec":{"pane_id":"%4","session_id":"hidden","surface_key":"stable-hidden",
                "tabs":[{"pane_id":"%8","session_id":"tab"}]}}
        ]});
        let current = json!({"stashed_panes":[
            {"pane_id":"%3","rec":{"pane_id":"%3","session_id":"hidden",
                "tabs":[{"pane_id":"%2","session_id":"tab"}]}},
            {"pane_id":"%1","rec":{"pane_id":"%1"}},
            {"pane_id":"%0","rec":null}
        ]});
        let prepared = prepare_restore_state(&current, Some(&previous));
        let stashed = &prepared["stashed_panes"];
        assert_eq!(stashed[0]["rec"]["surface_key"], "stable-hidden");
        assert_eq!(stashed[0]["rec"]["tabs"][0]["surface_key"], "legacy:%8");
        assert_eq!(stashed[1]["rec"]["surface_key"], "legacy:%1");
        assert!(stashed[0].get("surface_key").is_none());
        assert!(stashed[2]["rec"].is_null());
        assert_eq!(prepare_restore_state(&prepared, None), prepared);
    }

    #[test]
    fn missing_or_ambiguous_sessions_never_guess_old_number() {
        let previous = json!({"windows":[
            {"leaf":{"pane_id":"%4","session_id":"same"}},
            {"leaf":{"pane_id":"%8","session_id":"same"}},
            {"leaf":{"pane_id":"%7","session_id":"once"}}
        ]});
        let current = json!({"windows":[
            {"leaf":{"pane_id":"%0","session_id":"same"}},
            {"leaf":{"pane_id":"%1"}},
            {"leaf":{"pane_id":"%2","session_id":"once"}},
            {"leaf":{"pane_id":"%3","session_id":"once"}},
            {"leaf":{"pane_id":"%5","session_id":"unrelated"}}
        ]});
        let prepared = prepare_restore_state(&current, Some(&previous));
        for leaf in prepared["windows"].as_array().unwrap() {
            let rec = &leaf["leaf"];
            assert_eq!(rec["surface_key"], format!("legacy:{}", rec["pane_id"].as_str().unwrap()));
        }
    }
}
