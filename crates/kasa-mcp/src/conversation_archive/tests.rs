use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let fixture = Self(std::env::temp_dir().join(format!("kasa-archive-{}", uuid::Uuid::new_v4())));
        fixture.store().set_enabled("alice", true, &|| true).unwrap();
        fixture
    }
    fn store(&self) -> Store { Store::open(self.0.clone()).unwrap() }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn update(raw: &str) -> Append {
    Append { source: "session.jsonl".into(), surface_key: "pane-key".into(), title: "Conversation".into(), name: "Assistant".into(),
        harness: "claude".into(), cwd: "/project".into(), expected_revision: 0, from: 0, next: raw.len() as u64,
        reset: true, caught_up: true, raw: raw.into() }
}

#[test]
fn archives_survive_restart_without_a_desktop_and_are_account_scoped() {
    let fixture = Fixture::new();
    let entry = fixture.store().append("alice", "machine-a", "Desktop", update("first\n"), &|| true).unwrap();
    let store = fixture.store();
    assert_eq!(store.list("alice").unwrap(), vec![entry.clone()]);
    assert!(store.list("bob").unwrap().is_empty());
    assert_eq!(store.read("bob", &entry.id, &entry.generation, 0, entry.bytes).unwrap_err(), Error::Missing);
    assert_eq!(store.read("alice", &entry.id, &entry.generation, 0, entry.bytes).unwrap().raw, "first\n");
    assert!(store.list("../../alice").is_err());
}

#[test]
fn source_cursor_and_archive_bytes_are_distinct_and_retries_do_not_duplicate() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let mut first = update("visible\n"); first.next = 900;
    let entry = store.append("alice", "machine-a", "Desktop", first.clone(), &|| true).unwrap();
    assert_eq!(entry.cursor, 900); assert_eq!(entry.bytes, 8);
    assert_eq!(store.append("alice", "machine-a", "Desktop", first, &|| true).unwrap_err(), Error::Conflict);
    let mut next = update("new\n"); next.reset = false; next.from = 900; next.next = 1200; next.expected_revision = entry.revision;
    let newer = store.append("alice", "machine-a", "Desktop", next, &|| true).unwrap();
    assert_eq!(store.read("alice", &entry.id, &entry.generation, 0, entry.bytes).unwrap().raw, "visible\n");
    assert_eq!(store.read("alice", &entry.id, &entry.generation, entry.bytes, newer.bytes).unwrap().raw, "new\n");
}

#[test]
fn reset_invalidates_download_generation_and_deleted_records_need_explicit_restore() {
    let fixture = Fixture::new(); let mut store = fixture.store();
    let first = store.append("alice", "machine-a", "Desktop", update("old\n"), &|| true).unwrap();
    let mut reset = update("replacement\n"); reset.expected_revision = first.revision;
    let next = store.append("alice", "machine-a", "Desktop", reset, &|| true).unwrap();
    assert_ne!(first.generation, next.generation);
    assert_eq!(store.read("alice", &first.id, &first.generation, 0, first.bytes).unwrap_err(), Error::Conflict);
    store.delete("alice", &next.id, &|| true).unwrap();
    assert_eq!(store.append("alice", "machine-a", "Desktop", update("resurrect\n"), &|| true).unwrap_err(), Error::Missing);
    let restored = store.restore("alice", &next.id, &|| true).unwrap();
    assert!(!restored.ready && !restored.deleted && restored.cursor == 0);
    let mut fresh = update("fresh\n"); fresh.expected_revision = restored.revision;
    assert!(store.append("alice", "machine-a", "Desktop", fresh, &|| true).is_ok());
}

#[test]
fn partial_appends_stay_invisible_and_are_repaired_before_the_next_commit() {
    let fixture = Fixture::new(); let mut store = fixture.store();
    let first = store.append("alice", "machine-a", "Desktop", update("saved\n"), &|| true).unwrap();
    let path = store.data("alice", &first).unwrap();
    std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"uncommitted\n").unwrap();
    assert_eq!(fixture.store().read("alice", &first.id, &first.generation, 0, first.bytes).unwrap().raw, "saved\n");
    let mut next = update("next\n"); next.reset = false; next.from = first.cursor; next.next += first.cursor; next.expected_revision = first.revision;
    let next = store.append("alice", "machine-a", "Desktop", next, &|| true).unwrap();
    assert_eq!(store.read("alice", &next.id, &next.generation, 0, next.bytes).unwrap().raw, "saved\nnext\n");
}

#[test]
fn utf8_download_boundaries_do_not_change_bytes_or_mix_sources() {
    let fixture = Fixture::new(); let mut store = fixture.store();
    let raw = format!("{}\n", "한글".repeat(MAX_READ / 4));
    let first = store.append("alice", "machine-a", "Desktop", update(&raw), &|| true).unwrap();
    let mut text = String::new(); let mut offset = 0;
    while offset < first.bytes {
        let chunk = store.read("alice", &first.id, &first.generation, offset, first.bytes).unwrap();
        assert!(chunk.next > offset); offset = chunk.next; text.push_str(&chunk.raw);
    }
    assert_eq!(text, raw);
    let mut other = update("other\n"); other.source = "other-session.jsonl".into();
    let other = store.append("alice", "machine-a", "Desktop", other, &|| true).unwrap();
    assert_ne!(first.id, other.id);
    assert_ne!(id_for("machine-a", "session.jsonl"), id_for("machine-b", "session.jsonl"));
}

#[test]
fn rejected_writes_preserve_committed_history() {
    let fixture = Fixture::new(); let mut store = fixture.store();
    let first = store.append("alice", "machine-a", "Desktop", update("saved\n"), &|| true).unwrap();
    let mut bad = update("bad\n"); bad.expected_revision = first.revision;
    assert_eq!(store.append("alice", "machine-a", "Desktop", bad.clone(), &|| false).unwrap_err(), Error::Unauthorized);
    bad.raw = "x".repeat(MAX_APPEND + 1);
    assert_eq!(store.append("alice", "machine-a", "Desktop", bad, &|| true).unwrap_err(), Error::Invalid);
    assert_eq!(store.list("alice").unwrap(), vec![first.clone()]);
    assert_eq!(store.read("alice", &first.id, &first.generation, 0, first.bytes).unwrap().raw, "saved\n");
    store.set_enabled("alice", false, &|| true).unwrap();
    assert_eq!(store.append("alice", "machine-a", "Desktop", update("no\n"), &|| true).unwrap_err(), Error::Disabled);
    assert_eq!(store.read("alice", &first.id, &first.generation, 0, first.bytes).unwrap().raw, "saved\n");
}

#[cfg(unix)]
#[test]
fn symlinks_and_corrupt_manifests_are_not_followed_or_overwritten() {
    let fixture = Fixture::new(); let mut store = fixture.store();
    let first = store.append("alice", "machine-a", "Desktop", update("saved\n"), &|| true).unwrap();
    let manifest = store.account_dir("alice").unwrap().join(format!("{}.json", first.id));
    std::fs::write(&manifest, "broken").unwrap();
    assert_eq!(store.list("alice").unwrap_err(), Error::Storage);
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), "broken");
    let link = fixture.0.join("bob");
    std::os::unix::fs::symlink(store.account_dir("alice").unwrap(), link).unwrap();
    assert_eq!(store.list("bob").unwrap_err(), Error::Storage);
}
