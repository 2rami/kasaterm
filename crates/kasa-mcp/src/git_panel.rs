use kasa_socket::backend::Backend;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const SCHEMA: &str = "kasa.git-panel.v2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub machine_id: String,
    pub pane: String,
    pub surface_key: String,
    pub cwd: String,
}

pub fn absolute_path(path: &str) -> bool {
    !path.chars().any(char::is_control)
        && (path.starts_with('/') || path.starts_with("\\\\")
            || (path.as_bytes().get(1) == Some(&b':')
                && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && matches!(path.as_bytes().get(2), Some(b'\\' | b'/'))))
}

fn resolve(backend: &dyn Backend, query: &HashMap<String, String>) -> Result<Source, &'static str> {
    let machine_id = crate::mobile::machine_identity().ok_or("source_missing")?;
    if query.get("machine_id") != Some(&machine_id) { return Err("source_changed"); }
    let pane = query.get("pane").filter(|p| !p.is_empty()).ok_or("source_missing")?;
    let key = query.get("surface_key").filter(|k| !k.is_empty()).ok_or("update_needed")?;
    if crate::surface_keys::get(pane).as_ref() != Some(key) { return Err("source_changed"); }
    if crate::remote::remote_info(pane).is_some() { return Err("mirror_source"); }
    let cwd = backend.pane_cwds().into_iter().find(|(id, _)| id == pane)
        .map(|(_, cwd)| cwd).filter(|cwd| absolute_path(cwd)).ok_or("cwd_unavailable")?;
    if query.get("cwd").is_some_and(|expected| expected != &cwd) { return Err("source_changed"); }
    Ok(Source { machine_id, pane: pane.clone(), surface_key: key.clone(), cwd })
}

pub fn read(backend: &dyn Backend, query: &HashMap<String, String>) -> Value {
    let error = |code: &str| json!({"schema": SCHEMA, "ok": false, "error": code});
    if query.get("schema").map(String::as_str) != Some(SCHEMA) { return error("update_needed"); }
    let source = match resolve(backend, query) { Ok(source) => source, Err(code) => return error(code) };
    let commits = query.get("commits").and_then(|s| s.parse().ok()).unwrap_or(20usize).clamp(1, 200);
    let view = match crate::git::git_panel_snapshot(std::path::Path::new(&source.cwd), commits) {
        Ok(view) => view,
        Err(_) => return error("git_unavailable"),
    };
    // A pane may be replaced or change directory while its Git subprocess runs.
    if resolve(backend, query).as_ref() != Ok(&source) { return error("source_changed"); }
    json!({"schema": SCHEMA, "ok": true, "source": source, "view": view})
}

pub fn validate_response(value: &Value, expected: &Source) -> Result<Value, &'static str> {
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) { return Err("update_needed"); }
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(match value.get("error").and_then(Value::as_str) {
            Some("update_needed") => "update_needed",
            Some("source_changed" | "source_missing" | "mirror_source") => "source_changed",
            Some("cwd_unavailable") => "cwd_unavailable",
            _ => "git_unavailable",
        });
    }
    let source: Source = serde_json::from_value(value.get("source").cloned().unwrap_or(Value::Null))
        .map_err(|_| "invalid_response")?;
    if source.machine_id != expected.machine_id || source.pane != expected.pane
        || source.surface_key != expected.surface_key || !absolute_path(&source.cwd)
        || (!expected.cwd.is_empty() && source.cwd != expected.cwd) { return Err("source_changed"); }
    let view = value.get("view").filter(|v| v.is_object()).ok_or("invalid_response")?;
    if view.get("cwd").and_then(Value::as_str) != Some(source.cwd.as_str())
        || view.get("branch_list").and_then(Value::as_array).is_none()
        || view.get("no_repo").and_then(Value::as_bool).is_none() { return Err("invalid_response"); }
    Ok(view.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source { machine_id: "machine-a".into(), pane: "%4".into(), surface_key: "stable-a".into(), cwd: "/repo".into() }
    }

    fn response(source: &Source) -> Value {
        json!({"schema": SCHEMA, "ok": true, "source": source,
            "view": {"cwd": source.cwd, "branch_list": [], "no_repo": false}})
    }

    #[test]
    fn old_server_never_becomes_a_non_repository_snapshot() {
        assert_eq!(validate_response(&json!({"ok": false, "error": "path required"}), &source()), Err("update_needed"));
        assert_eq!(validate_response(&json!({"ok": true, "view": {"cwd": "/repo"}}), &source()), Err("update_needed"));
    }

    #[test]
    fn response_must_match_device_surface_and_directory() {
        let expected = source();
        assert!(validate_response(&response(&expected), &expected).is_ok());
        for field in ["machine_id", "pane", "surface_key", "cwd"] {
            let mut value = response(&expected);
            value["source"][field] = json!("wrong");
            assert_eq!(validate_response(&value, &expected), Err("source_changed"));
        }
        let mut value = response(&expected);
        value["view"]["cwd"] = json!("/other-repo");
        assert_eq!(validate_response(&value, &expected), Err("invalid_response"));
    }

    #[test]
    fn source_paths_support_windows_without_local_path_interpretation() {
        for path in ["/work/repo", "C:\\work\\repo", "D:/work/repo", "\\\\host\\share\\repo"] { assert!(absolute_path(path)); }
        for path in ["repo", "C:repo", "/repo\nother", ""] { assert!(!absolute_path(path)); }
    }
}
