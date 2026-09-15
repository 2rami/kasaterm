//! Only previously observed local transcripts provide deletion evidence. UUIDs
//! absent from local disk may belong to another machine and are never guessed.
use super::{read_session_chars_strict, write_session_chars_atomic};
use serde_json::{Map, Value};
use std::{collections::{HashMap, HashSet}, io, path::{Path, PathBuf}};

fn exists(path: &Path) -> io::Result<bool> { path.try_exists() }

fn files(root: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    if !exists(root)? { return Ok(()); }
    if std::fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(io::Error::other("linked session store; registry preserved"));
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() { return Err(io::Error::other("unscanned linked session store")); }
        if kind.is_dir() { files(&entry.path(), out)?; }
        else if kind.is_file() { out.push(entry.path()); }
        else { return Err(io::Error::other("unsupported session store entry")); }
    }
    Ok(())
}

fn uuid(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok()
}

fn references(value: &Value, ids: &mut HashSet<String>) -> io::Result<()> {
    match value {
        Value::String(s) => { if uuid(s) { ids.insert(s.clone()); } }
        Value::Array(items) => for item in items { references(item, ids)?; },
        Value::Object(items) => for (key, item) in items {
            // A moved session can remain resumable outside every local store.
            if (key.contains("remote") || key.contains("machine") || key == "ssh")
                && !item.is_null() && item != &Value::Bool(false) && item != "" {
                return Err(io::Error::other("remote session reference; registry preserved"));
            }
            references(item, ids)?;
        },
        _ => {}
    }
    Ok(())
}

fn inventory(home: &Path, config: &Path) -> io::Result<(HashMap<String, PathBuf>, HashSet<String>)> {
    // Account/custom homes and machine handoffs need a broader inventory than
    // this collector supports. Their presence makes absence inconclusive.
    for name in ["machines.json", "claude-accounts", "codex-accounts", "codex-homes"] {
        if exists(&config.join(name))? { return Err(io::Error::other("additional session store; registry preserved")); }
    }
    let mut transcripts = HashMap::new();
    for root in [home.join(".claude/projects"), home.join(".codex/sessions"), home.join(".codex/archived_sessions")] {
        if !exists(&root)? { return Err(io::Error::other("session store unavailable; registry preserved")); }
        let mut paths = Vec::new();
        files(&root, &mut paths)?;
        for path in paths {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") { continue; }
            let id = if uuid(stem) { Some(stem) }
                else if stem.starts_with("rollout-") { stem.get(stem.len().saturating_sub(36)..).filter(|s| uuid(s)) }
                else { None };
            if let Some(id) = id {
                // Opening detects access failures without loading conversation contents.
                std::fs::File::open(&path)?;
                transcripts.insert(id.to_owned(), path);
            }
        }
    }
    let mut ids = HashSet::new();
    let mut state = Vec::new();
    if !exists(&home.join(".claude/sessions"))? { return Err(io::Error::other("live registry unavailable")); }
    files(&home.join(".claude/sessions"), &mut state)?;
    for dir in [config.to_path_buf(), config.join("sessions")] {
        if !exists(&dir)? { continue; }
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "session.json" || (name.starts_with("session-restored-") && name.ends_with(".json")) {
                state.push(entry.path());
            }
        }
    }
    for path in state {
        if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
        let value: Value = serde_json::from_slice(&std::fs::read(path)?).map_err(io::Error::other)?;
        references(&value, &mut ids)?;
    }
    Ok((transcripts, ids))
}

pub(super) fn sweep(home: &Path, config: &Path, path: &Path) -> io::Result<usize> {
    let snapshot = read_session_chars_strict(path)?;
    let (transcripts, protected) = inventory(home, config)?;
    let guard = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(path.with_extension("json.lock"))?;
    guard.lock()?;
    // Recheck references under the same lock as bind, then merge the latest map.
    let (latest_transcripts, latest_protected) = inventory(home, config)?;
    let mut current = read_session_chars_strict(path)?;
    let ledger_path = path.with_file_name("session-character-observations.json");
    let mut ledger = read_session_chars_strict(&ledger_path)?;
    let mut removed = Map::new();
    for (id, name) in &snapshot {
        if !uuid(id) || protected.contains(id) || latest_protected.contains(id) || current.get(id) != Some(name) { continue; }
        if let Some(original) = ledger.get(id) {
            let old_name = original.get("character");
            let old_path = original.get("transcript").and_then(Value::as_str).map(PathBuf::from);
            if old_name == Some(name) && !transcripts.contains_key(id) && !latest_transcripts.contains_key(id) {
                if let Some(old_path) = old_path {
                    // A missing mount/project directory is not proof of deletion.
                    if old_path.parent().is_some_and(|p| p.is_dir()) && !exists(&old_path)? {
                        if let Some(value) = current.remove(id) { removed.insert(id.clone(), value); }
                        ledger.remove(id);
                    }
                }
            }
        }
    }
    for (id, transcript) in transcripts {
        if let Some(name) = current.get(&id) {
            ledger.insert(id, serde_json::json!({"character": name, "transcript": transcript}));
        }
    }
    ledger.retain(|id, _| current.contains_key(id));
    if !removed.is_empty() {
        // One bounded recovery file always contains the entire pre-prune map.
        write_session_chars_atomic(&path.with_file_name("session-characters-before-cleanup.json"), &read_session_chars_strict(path)?)?;
        write_session_chars_atomic(path, &current)?;
    }
    write_session_chars_atomic(&ledger_path, &ledger)?;
    Ok(removed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf, PathBuf, String) {
        let home = std::env::temp_dir().join(format!("kasaterm-gc-{}", uuid::Uuid::new_v4()));
        let config = home.join(".config/kasaterm");
        std::fs::create_dir_all(config.join("sessions")).unwrap();
        let path = config.join("sessions/session_characters.json");
        let id = uuid::Uuid::new_v4().to_string();
        std::fs::create_dir_all(home.join(".claude/projects/project")).unwrap();
        for dir in [".claude/sessions", ".codex/sessions", ".codex/archived_sessions"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        std::fs::write(home.join(format!(".claude/projects/project/{id}.jsonl")), "").unwrap();
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({id.clone(): "one", "unknown": "two"})).unwrap()).unwrap();
        (home, config, path, id)
    }
    #[test]
    fn only_observed_deleted_local_transcripts_are_pruned() {
        let (home, config, path, id) = fixture();
        assert_eq!(sweep(&home, &config, &path).unwrap(), 0);
        std::fs::remove_file(home.join(format!(".claude/projects/project/{id}.jsonl"))).unwrap();
        assert_eq!(sweep(&home, &config, &path).unwrap(), 1);
        assert!(read_session_chars_strict(&path).unwrap().contains_key("unknown"));
        assert!(read_session_chars_strict(&path.with_file_name("session-characters-before-cleanup.json")).unwrap().contains_key(&id));
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn restored_reference_and_unknown_origins_are_preserved() {
        let (home, config, path, id) = fixture();
        sweep(&home, &config, &path).unwrap();
        std::fs::remove_file(home.join(format!(".claude/projects/project/{id}.jsonl"))).unwrap();
        std::fs::write(config.join("sessions/session-restored-1.json"), format!(r#"{{"session_id":"{id}"}}"#)).unwrap();
        assert_eq!(sweep(&home, &config, &path).unwrap(), 0);
        assert_eq!(read_session_chars_strict(&path).unwrap().len(), 2);
        std::fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn unreadable_store_and_remote_configuration_fail_closed() {
        let (home, config, path, id) = fixture();
        sweep(&home, &config, &path).unwrap();
        std::fs::remove_file(home.join(format!(".claude/projects/project/{id}.jsonl"))).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::remove_dir(home.join(".codex/sessions")).unwrap();
        std::fs::write(home.join(".codex/sessions"), "not a directory").unwrap();
        assert!(sweep(&home, &config, &path).is_err());
        assert!(read_session_chars_strict(&path).unwrap().contains_key(&id));
        std::fs::remove_file(home.join(".codex/sessions")).unwrap();
        std::fs::create_dir(home.join(".codex/sessions")).unwrap();
        std::fs::write(config.join("machines.json"), "{}").unwrap();
        assert!(sweep(&home, &config, &path).is_err());
        assert!(read_session_chars_strict(&path).unwrap().contains_key(&id));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn archived_transcripts_unobserved_uuids_and_new_bindings_survive() {
        let (home, config, path, id) = fixture();
        sweep(&home, &config, &path).unwrap();
        let archive = home.join(".codex/archived_sessions");
        std::fs::create_dir_all(&archive).unwrap();
        let archived = archive.join(format!("rollout-2026-09-15-{id}.jsonl"));
        std::fs::rename(home.join(format!(".claude/projects/project/{id}.jsonl")), &archived).unwrap();
        assert_eq!(sweep(&home, &config, &path).unwrap(), 0);
        std::fs::remove_file(archived).unwrap();
        super::super::bind_session_character_in(&path, &id, "new binding").unwrap();
        let unseen = uuid::Uuid::new_v4().to_string();
        super::super::bind_session_character_in(&path, &unseen, "remote or unknown").unwrap();
        assert_eq!(sweep(&home, &config, &path).unwrap(), 0);
        let saved = read_session_chars_strict(&path).unwrap();
        assert_eq!(saved[&id], "new binding");
        assert!(saved.contains_key(&unseen));
        std::fs::remove_dir_all(home).unwrap();
    }
}
