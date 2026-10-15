use serde_json::{json, Value};

pub(super) fn is_local(record: &Value) -> bool {
    let text = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    if let Some(_base) = text("remote_base") {
        // Promotion hands this machine's PTY to its own daemon, not to another device.
        #[cfg(unix)]
        if record["remote_view"] != true
            && text("remote_label").is_none()
            && _base
                .trim_end_matches('/')
                .strip_prefix("http://127.0.0.1:")
                .and_then(|port| port.parse::<u16>().ok())
                == Some(super::migrate::LOCAL_PTYD_PORT)
        {
            return true;
        }
        return false;
    }
    text("remote_pane").is_none()
        && text("remote_label").is_none()
        && text("remote_surface_key").is_none()
        && record["remote_view"] != true
}

pub(super) fn surfaces(record: &Value) -> impl Iterator<Item = &Value> {
    std::iter::once(record)
        .chain(
            record
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .enumerate()
        .filter(|(index, record)| (*index == 0 || record.is_object()) && is_local(record))
        .map(|(_, record)| record)
}

pub(super) fn record(original: &Value) -> Option<Value> {
    record_with(original, &mut |_| true)
}

fn record_with(original: &Value, keep: &mut impl FnMut(&Value) -> bool) -> Option<Value> {
    let all: Vec<_> = std::iter::once(original)
        .chain(
            original
                .get("tabs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .collect();
    let kept: Vec<_> = all
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, r)| (*index == 0 || r.is_object()) && is_local(r) && keep(r))
        .collect();
    let (first_index, first) = kept.first()?;
    if kept.len() == all.len() {
        return Some(original.clone());
    }
    let mut result = (**first).clone();
    let object = result.as_object_mut()?;
    object.remove("tabs");
    object.remove("active_tab");
    if kept.len() > 1 {
        object.insert(
            "tabs".into(),
            Value::Array(kept.iter().skip(1).map(|(_, r)| (*r).clone()).collect()),
        );
    }
    let active = original["active_tab"]
        .as_u64()
        .unwrap_or(*first_index as u64) as usize;
    object.insert(
        "active_tab".into(),
        json!(kept
            .iter()
            .position(|(index, _)| *index == active)
            .unwrap_or(0)),
    );
    Some(result)
}

fn tree(node: &Value, keep: &mut impl FnMut(&Value) -> bool) -> Option<Value> {
    if crate::internal_room::is_saved_window(node) {
        return Some(node.clone());
    }
    if let Some(leaf) = node.get("leaf") {
        let mut result = node.clone();
        result["leaf"] = record_with(leaf, keep)?;
        return Some(result);
    }
    let Some(split) = node.get("split") else {
        return Some(node.clone());
    };
    let a = split.get("a").and_then(|node| tree(node, keep));
    let b = split.get("b").and_then(|node| tree(node, keep));
    match (a, b) {
        (Some(a), Some(b)) => {
            let mut result = node.clone();
            result["split"]["a"] = a;
            result["split"]["b"] = b;
            Some(result)
        }
        (Some(mut only), None) | (None, Some(mut only)) => {
            // Room metadata belongs to the room, even when its former first split disappears.
            if let (Some(parent), Some(child)) = (node.as_object(), only.as_object_mut()) {
                for (key, value) in parent.iter().filter(|(key, _)| key.as_str() != "split") {
                    child.insert(key.clone(), value.clone());
                }
            }
            Some(only)
        }
        (None, None) => None,
    }
}

fn mapped(index: &Value, indices: &[Option<usize>]) -> usize {
    index
        .as_u64()
        .and_then(|index| indices.get(index as usize))
        .copied()
        .flatten()
        .unwrap_or(0)
}

fn session(value: &mut Value, keep: &mut impl FnMut(&Value) -> bool) -> Vec<Option<usize>> {
    let Some(object) = value.as_object_mut() else {
        return Vec::new();
    };
    let mut indices = Vec::new();
    if let Some(windows) = object.get_mut("windows").and_then(Value::as_array_mut) {
        let mut kept = Vec::new();
        for window in std::mem::take(windows) {
            if let Some(window) = tree(&window, keep) {
                indices.push(Some(kept.len()));
                kept.push(window);
            } else {
                indices.push(None);
            }
        }
        *windows = kept;
    }
    if let Some(active) = object.get_mut("active_window") {
        *active = json!(mapped(active, &indices));
    }
    if let Some(bodies) = object.get_mut("room_body").and_then(Value::as_object_mut) {
        *bodies = std::mem::take(bodies)
            .into_iter()
            .filter_map(|(key, value)| {
                let index = indices.get(key.parse::<usize>().ok()?).copied().flatten()?;
                Some((index.to_string(), value))
            })
            .collect();
    }
    if let Some(collapsed) = object
        .get_mut("collapsed_rooms")
        .and_then(Value::as_array_mut)
    {
        *collapsed = collapsed
            .iter()
            .filter_map(|index| {
                Some(json!(indices
                    .get(index.as_u64()? as usize)
                    .copied()
                    .flatten()?))
            })
            .collect();
    }
    if let Some(undocked) = object.get_mut("undocked").and_then(Value::as_array_mut) {
        *undocked = std::mem::take(undocked)
            .into_iter()
            .filter_map(|entry| {
                let mut entry = tree(&entry, keep)?;
                if let Some(home) = entry.get_mut("home_window") {
                    *home = json!(mapped(home, &indices));
                }
                Some(entry)
            })
            .collect();
    }
    indices
}

pub(super) fn state(original: &Value) -> Value {
    let mut result = original.clone();
    let active = super::restore_selection::active_index(&result);
    let mut active_indices = Vec::new();
    if let Some(sessions) = result.get_mut("sessions").and_then(Value::as_array_mut) {
        let selected = active;
        for (index, saved) in sessions.iter_mut().enumerate() {
            let indices = session(saved, &mut |_| true);
            if index == selected {
                active_indices = indices;
            }
        }
    }
    if let Some(stashed) = result
        .get_mut("stashed_panes")
        .and_then(Value::as_array_mut)
    {
        *stashed = std::mem::take(stashed)
            .into_iter()
            .filter_map(|mut entry| {
                let kept = record(entry.get("rec")?)?;
                if kept.get("pane_id") != entry["rec"].get("pane_id") {
                    entry["pane_id"] = kept["pane_id"].clone();
                    entry["character"] = kept["character"].clone();
                    entry["neighbor"] = Value::Null;
                }
                entry["rec"] = kept;
                if let Some(window) = entry.get_mut("window") {
                    *window = json!(mapped(window, &active_indices));
                }
                Some(entry)
            })
            .collect();
    }
    result
}

pub(super) fn selected(original: &Value, chosen: &[usize]) -> Value {
    let mut result = state(original);
    if let Some(object) = result.as_object_mut() {
        object.remove("restore_selection");
    }
    let active = super::restore_selection::active_index(&result);
    let mut omitted = Vec::new();
    let mut index = 0;
    let mut keep = |rec: &Value| {
        let selected = chosen.contains(&index);
        index += 1;
        if !selected {
            let mut rec = rec.clone();
            if let Some(object) = rec.as_object_mut() {
                object.remove("tabs");
                object.remove("active_tab");
            }
            omitted.push(rec);
        }
        selected
    };
    let indices = result
        .get_mut("sessions")
        .and_then(Value::as_array_mut)
        .and_then(|sessions| sessions.get_mut(active))
        .map(|saved| session(saved, &mut keep))
        .unwrap_or_default();
    let mut stashed = result
        .get_mut("stashed_panes")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    for entry in &mut stashed {
        if let Some(window) = entry.get_mut("window") {
            *window = json!(mapped(window, &indices));
        }
    }
    // Each unchecked tab keeps its own launch recipe, never a parent record containing selected tabs.
    for rec in omitted {
        let pane = rec.get("pane_id").cloned().unwrap_or(Value::Null);
        let duplicate = stashed.iter().position(|entry| {
            let old = &entry["rec"];
            if let Some(key) = rec.get("surface_key").and_then(Value::as_str) {
                old.get("surface_key").and_then(Value::as_str) == Some(key)
            } else {
                !pane.is_null()
                    && old.get("pane_id") == Some(&pane)
                    && old.get("session_id") == rec.get("session_id")
            }
        });
        if let Some(index) = duplicate {
            stashed.remove(index);
        }
        let agent = super::restore_selection::agent_kind(&rec).is_some();
        stashed.push(json!({
            "pane_id": pane, "character": if agent { rec["character"].as_str().unwrap_or("") } else { "" },
            "folder": rec["cwd"].as_str().unwrap_or(""), "neighbor": null,
            "window": 0, "stashed": true, "retained_by": "deferred_restore", "rec": rec,
        }));
    }
    if !stashed.is_empty() {
        result["stashed_panes"] = json!(stashed);
    }
    result
}

pub(super) fn history(original: &Value) -> Vec<crate::ClosedPane> {
    let filtered = state(original);
    filtered
        .get("stashed_panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let rec = entry.get("rec")?.clone();
            if rec.is_null() {
                return None;
            }
            let text = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            Some(crate::ClosedPane {
                rec,
                pane_id: text("pane_id"),
                character: text("character"),
                folder: text("folder"),
                neighbor: entry
                    .get("neighbor")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                window: entry["window"].as_u64().unwrap_or(0) as usize,
                alive: false,
                stashed: entry["stashed"].as_bool().unwrap_or(true),
                retained_by: entry
                    .get("retained_by")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok()),
                idle_since: None,
                preview: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_reasons_round_trip_without_inventing_legacy_user_intent() {
        let state = json!({"stashed_panes":[
            {"rec":{},"pane_id":"%1","stashed":true,"retained_by":"user"},
            {"rec":{},"pane_id":"%2","stashed":true,"retained_by":"recovery"},
            {"rec":{},"pane_id":"%3","stashed":true},
            {"rec":{},"pane_id":"%4","stashed":false},
            {"rec":{},"pane_id":"%5","stashed":true,"retained_by":"future-value"}
        ]});
        let restored = history(&state);
        let labels: Vec<_> = restored
            .iter()
            .map(crate::ClosedPane::retention_label)
            .collect();
        assert_eq!(
            labels,
            [
                Some("직접 숨김"),
                Some("자동 보관"),
                Some("보관됨"),
                None,
                Some("보관됨")
            ]
        );
        for entry in &restored {
            assert!(!entry.alive);
        }
        for entry in restored {
            let again = history(&json!({"stashed_panes":[{
                "rec":entry.rec, "stashed":entry.stashed, "retained_by":entry.retained_by
            }]}));
            assert_eq!(again[0].retained_by, entry.retained_by);
            assert_eq!(again[0].stashed, entry.stashed);
        }
    }

    fn remote(id: &str) -> Value {
        json!({"pane_id":id,"remote_base":"http://127.0.0.1:18765","remote_pane":"%2",
            "remote_view":true,"remote_owned":true,"session_id":"remote-sid","character":"remote"})
    }

    fn local(id: &str) -> Value {
        json!({"pane_id":id,"surface_key":format!("key-{id}"),"session_id":format!("sid-{id}"),
            "was_agent":"codex","character":"local","cwd":"/work","model":"gpt-6.1"})
    }

    #[test]
    fn local_tab_under_a_mirror_keeps_its_own_identity_and_selected_tab() {
        let mut input = remote("%0");
        input["tabs"] = json!([local("%1"), remote("%2"), local("%3")]);
        input["active_tab"] = json!(3);
        let result = record(&input).unwrap();
        for key in [
            "pane_id",
            "surface_key",
            "session_id",
            "character",
            "cwd",
            "model",
        ] {
            assert_eq!(result[key], local("%1")[key], "{key}");
        }
        assert!(result.get("remote_base").is_none());
        assert_eq!(result["tabs"], json!([local("%3")]));
        assert_eq!(result["active_tab"], 1);
        assert_eq!(record(&result).unwrap(), result);
        input["active_tab"] = json!(2);
        assert_eq!(record(&input).unwrap()["active_tab"], 0);
    }

    #[test]
    fn missing_endpoint_and_owned_remote_shells_cannot_turn_into_local_sessions() {
        let mut input = remote("%0");
        input.as_object_mut().unwrap().remove("remote_base");
        assert!(record(&input).is_none());
        input = remote("%0");
        input["remote_view"] = json!(false);
        assert!(record(&input).is_none());
    }

    #[test]
    fn a_missing_tab_is_not_a_local_session_but_cannot_hide_a_later_local_tab() {
        let mut input = remote("%0");
        input["tabs"] = json!([null]);
        assert!(record(&input).is_none());
        assert!(surfaces(&input).next().is_none());
        input["tabs"] = json!([null, local("%2")]);
        assert_eq!(record(&input).unwrap()["pane_id"], "%2");
    }

    #[cfg(unix)]
    #[test]
    fn this_machines_promoted_pty_is_not_a_foreign_device_mirror() {
        let mut input = remote("%0");
        input["remote_base"] = json!(format!(
            "http://127.0.0.1:{}/",
            super::super::migrate::LOCAL_PTYD_PORT
        ));
        input["remote_view"] = json!(false);
        assert_eq!(record(&input), Some(input.clone()));
        input["remote_view"] = json!(true);
        assert!(record(&input).is_none());
    }

    #[test]
    fn foreign_rooms_disappear_and_all_room_indices_follow_local_survivors() {
        let input = json!({"sessions":[{"windows":[
            {"leaf":remote("%0")}, {"leaf":local("%1")},
            {"split":{"dir":"h","ratio":0.3,"a":{"leaf":remote("%2")},"b":{"leaf":local("%3")}},"name":"mixed"}
        ],"active_window":2,"room_body":{"0":"map","1":"list","2":"map"},
        "collapsed_rooms":[0,2],"remote_collapsed_rooms":["mini:window:2"],
        "undocked":[{"leaf":remote("%4"),"home_window":0},{"leaf":local("%5"),"home_window":2,"frame":{"x":10}}]}],
        "stashed_panes":[{"rec":remote("%6"),"window":0},{"rec":local("%7"),"window":2}],
        "last_used_unix":100});
        let result = state(&input);
        let s = &result["sessions"][0];
        assert_eq!(s["windows"].as_array().unwrap().len(), 2);
        assert_eq!(s["windows"][1]["leaf"], local("%3"));
        assert_eq!(s["windows"][1]["name"], "mixed");
        assert_eq!(s["active_window"], 1);
        assert_eq!(s["room_body"], json!({"0":"list","1":"map"}));
        assert_eq!(s["collapsed_rooms"], json!([1]));
        assert_eq!(
            s["remote_collapsed_rooms"],
            input["sessions"][0]["remote_collapsed_rooms"]
        );
        assert_eq!(
            s["undocked"],
            json!([{"leaf":local("%5"),"home_window":1,"frame":{"x":10}}])
        );
        assert_eq!(
            result["stashed_panes"],
            json!([{"rec":local("%7"),"window":1}])
        );
        assert_eq!(result["last_used_unix"], 100);
        assert_eq!(state(&result), result);
        assert_eq!(input["sessions"][0]["windows"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn remote_only_snapshot_has_no_restore_work_and_local_snapshot_is_unchanged() {
        let remote = json!({"sessions":[{"windows":[{"leaf":remote("%0")}],"active_window":0}]});
        let filtered = state(&remote);
        assert_eq!(super::super::App::count_panes(&remote), 0);
        assert_eq!(super::super::App::count_claude_panes(&remote), 0);
        assert!(super::super::App::saved_characters(&remote).is_empty());
        assert!(filtered["sessions"][0]["windows"]
            .as_array()
            .unwrap()
            .is_empty());
        let local = json!({"sessions":[{"windows":[{"leaf":local("%1")}],"active_window":0}]});
        assert_eq!(state(&local), local);
    }

    #[test]
    fn closed_local_history_survives_without_a_restore_prompt_or_new_process() {
        let input = json!({"sessions":[{"windows":[{"leaf":remote("%0")}]}],
            "stashed_panes":[{"rec":remote("%1"),"pane_id":"%1"},
                {"rec":local("%2"),"pane_id":"%2","window":0,"stashed":true}]});
        assert_eq!(super::super::App::count_panes(&input), 0);
        let history = history(&input);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].rec, local("%2"));
        assert_eq!(history[0].pane_id, "%2");
        assert!(!history[0].alive);
        assert!(history[0].stashed);
    }

    #[test]
    fn a_local_document_survives_the_remote_primary_without_shell_metadata() {
        let mut input = remote("%0");
        let doc = json!({"pane_id":"%1","web_url":"http://localhost:3000","title":"preview"});
        input["tabs"] = json!([doc]);
        let result = record(&input).unwrap();
        assert_eq!(result["web_url"], "http://localhost:3000");
        assert!(result.get("session_id").is_none());
        assert!(result.get("remote_pane").is_none());
    }
}
