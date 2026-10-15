use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub title: String,
    pub kind: String,
    pub character: String,
    pub icon: &'static str,
    pub folder: String,
    pub identity: String,
    pub detail: String,
    pub group: usize,
    pub tab: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub title: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Default)]
pub(crate) struct Plan {
    pub rows: Vec<Row>,
    pub groups: Vec<Group>,
    pub saved_at: String,
}

pub(super) fn active_index(state: &Value) -> usize {
    let active = state["active_session"].as_u64().unwrap_or(0) as usize;
    if state["sessions"]
        .as_array()
        .is_some_and(|s| active < s.len())
    {
        active
    } else {
        0
    }
}

fn text<'a>(rec: &'a Value, key: &str) -> Option<&'a str> {
    rec.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

pub(super) fn agent_kind(rec: &Value) -> Option<&str> {
    super::restore::saved_agent_marker(rec)
        .or_else(|| text(rec, "session_id").is_some().then_some("에이전트"))
}

impl Plan {
    pub(crate) fn new(original: &Value) -> Self {
        let state = super::local_restore::state(original);
        let mut plan = Self {
            saved_at: crate::restore_progress::last_used_label(&state),
            ..Self::default()
        };
        let session = &state["sessions"][active_index(&state)];
        for (key, label) in [("windows", "방"), ("undocked", "별도 창")] {
            for (index, node) in session[key].as_array().into_iter().flatten().enumerate() {
                if crate::internal_room::is_saved_window(node) {
                    continue;
                }
                let start = plan.rows.len();
                let group = plan.groups.len();
                plan.node(node, group);
                if plan.rows.len() == start {
                    continue;
                }
                let name = text(node, "restore_label").map(str::to_owned).or_else(|| {
                    plan.rows.get(start).and_then(|row| {
                        row.folder
                            .rsplit(['/', '\\'])
                            .find(|s| !s.is_empty())
                            .map(str::to_owned)
                    })
                });
                plan.groups.push(Group {
                    title: format!(
                        "{label} {}{}",
                        index + 1,
                        name.map(|s| format!(" · {s}")).unwrap_or_default()
                    ),
                    start,
                    end: plan.rows.len(),
                });
            }
        }
        plan
    }

    fn node(&mut self, node: &Value, group: usize) {
        if let Some(rec) = node.get("leaf") {
            for (tab, rec) in super::local_restore::surfaces(rec).enumerate() {
                let agent = agent_kind(rec);
                let item = super::restore_dialog::Item::from_record(rec, agent.is_some());
                let kind = match agent {
                    Some("claude") => "Claude".into(),
                    Some("codex") => "Codex".into(),
                    Some(other) => other.to_owned(),
                    None if rec.get("web_url").is_some() => "웹".into(),
                    None if rec.get("server").is_some() => "서버".into(),
                    None => "터미널".into(),
                };
                let folder = text(rec, "cwd").unwrap_or("").to_owned();
                let title = text(rec, "title")
                    .or_else(|| text(rec, "restore_title"))
                    .or_else(|| rec.get("server").and_then(|s| text(s, "name")))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        let project = folder.rsplit(['/', '\\']).find(|s| !s.is_empty());
                        project
                            .map(|p| format!("{p} · {kind}"))
                            .unwrap_or_else(|| kind.clone())
                    });
                let mut identity = kind.clone();
                if agent.is_some() {
                    if let Some(name) = text(rec, "character") {
                        identity.push_str(&format!(" · {}", name));
                    }
                }
                if let Some(pane) = text(rec, "pane_id") {
                    identity.push_str(&format!(" · {pane}"));
                }
                let mut details = Vec::new();
                if let Some(model) = text(rec, "model") {
                    details.push(model.to_owned());
                }
                if let Some(sid) = text(rec, "session_id") {
                    details.push(format!("세션 {sid}"));
                }
                if details.is_empty() {
                    details.push("저장된 작업 공간".into());
                }
                self.rows.push(Row {
                    title,
                    kind,
                    character: item.character,
                    icon: item.icon,
                    folder,
                    identity,
                    detail: details.join(" · "),
                    group,
                    tab,
                });
            }
        } else if let Some(split) = node.get("split") {
            for side in ["a", "b"] {
                if let Some(child) = split.get(side) {
                    self.node(child, group);
                }
            }
        }
    }
}

pub(crate) fn apply(state: &Value) -> Value {
    let Some(chosen) = state.get("restore_selection").and_then(Value::as_array) else {
        return state.clone();
    };
    let chosen: Vec<_> = chosen
        .iter()
        .filter_map(Value::as_u64)
        .map(|i| i as usize)
        .collect();
    super::local_restore::selected(state, &chosen)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> Value {
        json!({"active_session":0, "sessions":[{
            "active_window":1, "room_body":{"0":"canvas","1":"terminal"}, "collapsed_rooms":[0,1],
            "windows":[
                {"restore_label":"Web app", "split":{"dir":"horizontal","ratio":0.6,
                    "a":{"leaf":{"pane_id":"%1","surface_key":"one","title":"API","cwd":"/tmp/web","was_agent":"codex","session_id":"session-one","model":"model-one","active_tab":2,
                        "tabs":[{"pane_id":"%2","surface_key":"two","title":"API","cwd":"/tmp/web","was_agent":"claude","session_id":"session-two"},
                                {"pane_id":"%3","surface_key":"three","title":"Logs","cwd":"/tmp/web"}]}},
                    "b":{"leaf":{"pane_id":"%4","surface_key":"four","title":"Server","cwd":"/tmp/web","server":{"name":"dev"}}}}},
                {"leaf":{"pane_id":"%5","surface_key":"five","cwd":"/tmp/docs","restore_title":"Design notes"}}
            ], "undocked":[{"home_window":1,"frame":{"x":100},"leaf":{"pane_id":"%6","surface_key":"six","cwd":"/tmp/docs"}}]
        }], "stashed_panes":[{"rec":{"pane_id":"%7","surface_key":"seven"},"pane_id":"%7","window":1,"stashed":true,"retained_by":"user"}]})
    }

    fn choose(mut state: Value, selected: &[usize]) -> Value {
        state["restore_selection"] = json!(selected);
        apply(&state)
    }

    #[test]
    fn selected_tabs_keep_identity_and_active_tab_while_unchecked_work_is_retained() {
        let original = snapshot();
        let plan = Plan::new(&original);
        assert_eq!(plan.rows.len(), 6);
        assert_eq!(plan.groups.len(), 3);
        assert_eq!(plan.rows[0].title, plan.rows[1].title);
        assert_ne!(plan.rows[0].identity, plan.rows[1].identity);
        let result = choose(original.clone(), &[1, 2, 5]);
        let session = &result["sessions"][0];
        assert_eq!(session["windows"].as_array().unwrap().len(), 1);
        let window = &session["windows"][0];
        assert_eq!(window["restore_label"], "Web app");
        assert_eq!(window["leaf"]["surface_key"], "two");
        assert_eq!(window["leaf"]["session_id"], "session-two");
        assert_eq!(window["leaf"]["active_tab"], 1);
        assert_eq!(window["leaf"]["tabs"][0]["pane_id"], "%3");
        assert!(window.get("split").is_none());
        assert_eq!(session["active_window"], 0);
        assert_eq!(session["room_body"], json!({"0":"canvas"}));
        assert_eq!(session["collapsed_rooms"], json!([0]));
        assert_eq!(session["undocked"][0]["home_window"], 0);
        assert_eq!(session["undocked"][0]["frame"], json!({"x":100}));
        let stash = result["stashed_panes"].as_array().unwrap();
        assert_eq!(stash.len(), 4);
        assert_eq!(stash[0]["retained_by"], "user");
        assert_eq!(stash[0]["window"], 0);
        let first = &stash[1]["rec"];
        assert_eq!(first["session_id"], "session-one");
        assert_eq!(first["model"], "model-one");
        assert!(first.get("tabs").is_none());
        assert_eq!(stash[2]["rec"]["server"]["name"], "dev");
        for entry in &stash[1..] {
            assert_eq!(entry["retained_by"], "deferred_restore");
        }
        let history = super::super::local_restore::history(&result);
        assert_eq!(history.len(), 4);
        assert_eq!(history[1].retention_label(), Some("복원 보류"));
        assert_eq!(super::super::local_restore::state(&result), result);
        assert_eq!(original, snapshot());
        assert!(result.get("restore_selection").is_none());
    }

    #[test]
    fn all_selected_is_lossless_and_none_launches_nothing() {
        let original = snapshot();
        assert_eq!(choose(original.clone(), &[0, 1, 2, 3, 4, 5]), original);
        let none = choose(original, &[]);
        assert_eq!(crate::App::count_panes(&none), 0);
        assert_eq!(none["stashed_panes"].as_array().unwrap().len(), 7);
        assert!(Plan::new(&none).rows.is_empty());
    }

    #[test]
    fn local_tabs_and_detached_windows_use_exactly_the_displayed_indices() {
        let state = json!({"active_session":99,"sessions":[{"windows":[
            {"leaf":{"internal":"settings","pane_id":"\u{0000}kasaterm-settings"}},
            {"leaf":{"pane_id":"remote","remote_view":true,"tabs":[
                {"pane_id":"local","cwd":"/tmp","character":"old-shell-name"},
                {"pane_id":"remote-tab","remote_label":"other"}]}},
            {"leaf":{"pane_id":"real","was_claude":true,"character":"Student"}}
        ],"undocked":[{"leaf":{"pane_id":"detached","was_agent":"codex"}}]},
        {"windows":[{"leaf":{"pane_id":"inactive"}}]}]});
        let plan = Plan::new(&state);
        assert_eq!(plan.rows.len(), 3);
        assert_eq!(plan.rows[0].kind, "터미널");
        assert!(!plan.rows[0].identity.contains("old-shell-name"));
        assert_eq!(plan.rows[2].kind, "Codex");
        let result = choose(state, &[1, 2]);
        assert_eq!(Plan::new(&result).rows.len(), 2);
        assert_eq!(result["stashed_panes"][0]["pane_id"], "local");
        assert_eq!(result["stashed_panes"][0]["character"], "");
        assert_eq!(
            result["sessions"][1]["windows"][0]["leaf"]["pane_id"],
            "inactive"
        );
    }

    #[test]
    fn display_title_does_not_become_pinned_and_deferred_records_do_not_duplicate() {
        let mut original = snapshot();
        original["stashed_panes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"rec":{"pane_id":"%1","surface_key":"one","session_id":"session-one"},"stashed":true}));
        let result = choose(original, &[4]);
        assert_eq!(Plan::new(&result).rows[0].title, "Design notes");
        assert!(result["sessions"][0]["windows"][0]["leaf"]
            .get("title")
            .is_none());
        assert_eq!(result["stashed_panes"].as_array().unwrap().len(), 6);
    }
    #[test]
    fn reused_surface_with_another_conversation_keeps_both_histories() {
        let mut original = snapshot();
        original["stashed_panes"].as_array_mut().unwrap().push(json!({
            "rec":{"pane_id":"%1","surface_key":"one","session_id":"older-conversation"},"stashed":true
        }));
        let result = choose(original, &[4]);
        let stash = result["stashed_panes"].as_array().unwrap();
        assert_eq!(stash.len(), 7);
        assert!(stash
            .iter()
            .any(|e| e["rec"]["session_id"] == "older-conversation"));
        assert!(stash
            .iter()
            .any(|e| e["rec"]["session_id"] == "session-one"));
    }
}
