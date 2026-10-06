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

/// 보기 기기의 Git 열이 기다리는 한도. 원격 GET 이 10초에 끊고, 관문 우회는 답 머리를 20초까지만 기다린다.
pub const WAIT_CAP_MS: u64 = 8000;

/// 원본 칸의 Git 이 바뀌었을 수 있을 때까지 쥔다 — 그 칸이나 같은 작업 트리의 claude 가 파일을 고쳤거나 쓰는 명령을
/// 돌렸다(`claude_mod` 의 깃 신호). 보기 기기는 이 답이 오면 `read` 로 다시 읽는다. `since` 가 없으면 지금 번호를
/// 바로 준다. 답 `{schema, ok, seq, changed, live}` — 신호의 경로·명령은 내보내지 않는다.
pub async fn wait(backend: std::sync::Arc<dyn Backend>, query: HashMap<String, String>) -> Value {
    let error = |code: &str| json!({"schema": SCHEMA, "ok": false, "error": code});
    if query.get("schema").map(String::as_str) != Some(SCHEMA) { return error("update_needed"); }
    let since = query.get("since").and_then(|s| s.parse::<u64>().ok());
    let hold = query.get("wait_ms").and_then(|s| s.parse::<u64>().ok()).unwrap_or(WAIT_CAP_MS).min(WAIT_CAP_MS);
    let resolved = tokio::task::spawn_blocking(move || {
        let source = resolve(backend.as_ref(), &query)?;
        let cwd = std::path::PathBuf::from(&source.cwd);
        let root = crate::git::panel_repo_root(&cwd).unwrap_or(cwd);
        let live = crate::claude_mod::live(&source.pane).is_some();
        Ok::<_, &'static str>((source.pane, root, live))
    }).await;
    let (pane, root, live) = match resolved { Ok(Ok(found)) => found, Ok(Err(code)) => return error(code), Err(_) => return error("git_unavailable") };
    // `live` — 그 칸의 claude 가 mod 로 말한다. 보기 기기는 그 칸의 주기 조회를 늦춘다.
    let answer = |seq: u64, changed: bool| json!({"schema": SCHEMA, "ok": true, "seq": seq, "changed": changed, "live": live});
    let Some(mut seen) = since else { return answer(crate::claude_mod::git_seq(), false) };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(hold);
    loop {
        let (seq, signals, lost) = crate::claude_mod::git_signals_since(seen);
        // 번호가 줄었으면 원본 앱이 다시 떴다 — 그 사이 무엇이 바뀌었는지 모른다.
        if seen > seq || lost || signals.iter().any(|s| crate::claude_mod::git_signal_touches(s, &pane, &root)) {
            return answer(seq, true);
        }
        seen = seq;
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() { return answer(seq, false); }
        crate::claude_mod::wait_git(seen, left).await;
    }
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
