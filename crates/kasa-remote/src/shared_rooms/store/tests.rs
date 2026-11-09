use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine;
use kasa_socket::shared_rooms::{
    CreateFork, ForkCheckpoint, NativeForkSnapshot, ProjectSnapshot, PublishCheckpoint, PublishSession, SharedAppend, SharedFile,
    FORMAT_VERSION,
};

const DESK: Publisher<'static> = Publisher { account: "alice", machine_id: "alice-desk" };
const NOW: u64 = 1_000_000_000;

fn ok() -> bool { true }

struct Fixture { root: PathBuf, clock: Arc<AtomicU64> }

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kasa-shared-rooms-{}", uuid::Uuid::new_v4()));
        Self { root, clock: Arc::new(AtomicU64::new(NOW)) }
    }
    fn store(&self) -> Store {
        let clock = self.clock.clone();
        Store::with_clock(self.root.clone(), Box::new(move || clock.load(Ordering::SeqCst))).unwrap()
    }
    fn advance(&self, ms: u64) { self.clock.fetch_add(ms, Ordering::SeqCst); }
    fn room_dir(&self, room: &Room) -> PathBuf { self.root.join("rooms").join(room.id()) }
    fn content_file(&self, room: &Room) -> PathBuf {
        std::fs::read_dir(self.room_dir(room)).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "log")).unwrap()
    }
}

impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

fn room(store: &mut Store, owner: &str, allow_fork: bool) -> Room {
    store.create(owner, CreateRoom { title: "Pairing".into(), allow_fork, request_id: None }, &ok).unwrap()
}

fn join(store: &mut Store, room: &Room, account: &str) {
    let invite = store.invite(room.owner(), room.id(), CreateInvite::default(), &ok).unwrap();
    store.accept(account, &invite.token, &ok).unwrap();
}

fn publish(store: &mut Store, room: &Room) -> SharedSession {
    let request = PublishSession { source_key: "/Users/alice/.claude/projects/app/s.jsonl".into(), title: "Debugging".into(), harness: "claude".into() };
    store.publish(DESK, room.id(), request, &ok).unwrap()
}

fn append(session: &SharedSession, raw: &str, reset: bool) -> SharedAppend {
    let from = if reset { 0 } else { session.cursor };
    SharedAppend { expected_revision: session.revision, generation: session.generation.clone(), from, next: from + 10, reset, raw: raw.into(), caught_up: true }
}

fn read_all(store: &Store, account: &str, room: &Room, session: &str) -> Result<String, Error> {
    let mut range = ReadRange::default();
    let mut out = String::new();
    loop {
        let chunk = store.read(account, room.id(), session, &range, &ok)?;
        out.push_str(&chunk.raw);
        if chunk.next == chunk.end { return Ok(out); }
        range = ReadRange { generation: Some(chunk.generation), offset: chunk.next, end: Some(chunk.end) };
    }
}

fn checkpoint(harness: &str, line: &str) -> ForkCheckpoint {
    let jsonl = format!("{line}\n");
    ForkCheckpoint {
        format_version: FORMAT_VERSION,
        native: NativeForkSnapshot {
            harness: harness.into(), parent_session_id: "0b4d3f5e-1111-2222-3333-444455556666".into(), boundary_id: "msg-42".into(),
            boundary_label: "after the tests".into(), native_version: "2.1.0".into(), sha256: digest(jsonl.as_bytes()), jsonl,
        },
        captured_at_ms: NOW - 1000, display_raw: "display\n".into(), project: None, omissions: Vec::new(),
    }
}

/// Against the session's current state, as a publisher that just synced would send it.
fn cp(store: &Store, room: &Room, session: &str, checkpoint: ForkCheckpoint) -> PublishCheckpoint {
    let view = store.room(room.owner(), room.id(), &ok).unwrap().view(room.owner(), &str::to_string);
    let current = view.sessions.iter().find(|s| s.id == session).unwrap();
    PublishCheckpoint { expected_generation: current.generation.clone(), expected_revision: current.revision, checkpoint }
}

fn later(mut checkpoint: ForkCheckpoint, ms: u64) -> ForkCheckpoint {
    checkpoint.captured_at_ms += ms;
    checkpoint
}

fn file(path: &str, bytes: &[u8]) -> SharedFile {
    SharedFile { path: path.into(), bytes_base64: base64::engine::general_purpose::STANDARD.encode(bytes), sha256: digest(bytes) }
}

fn with_project(files: Vec<SharedFile>, repo_url: &str) -> ForkCheckpoint {
    let mut value = checkpoint("claude", r#"{"type":"user"}"#);
    value.project = Some(ProjectSnapshot {
        repo_url: Some(repo_url.into()), commit: Some("a".repeat(40)), captured_at_ms: NOW - 1000, patch: String::new(), files,
    });
    value
}

fn contains_anywhere(dir: &Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).any(|path| if path.is_dir() {
        contains_anywhere(&path, needle)
    } else {
        std::fs::read(&path).unwrap().windows(needle.len()).any(|w| w == needle)
    })
}

#[test]
fn rooms_are_visible_to_members_only_and_invite_tokens_are_never_stored() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    assert!(store.list("bob").unwrap().is_empty());
    assert_eq!(store.room("bob", shared.id(), &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.accept("carol", "not-a-token", &ok).err().unwrap(), Error::Invalid);
    let invite = store.invite("alice", shared.id(), CreateInvite { expires_in_secs: None, max_uses: Some(2) }, &ok).unwrap();
    assert!(!contains_anywhere(&fixture.root, invite.token.as_bytes()), "only the digest is stored");
    let mut forged = invite.token.clone();
    forged.replace_range(forged.len() - 1.., if forged.ends_with('0') { "1" } else { "0" });
    assert_eq!(store.accept("bob", &forged, &ok).err().unwrap(), Error::Gone);
    let joined = store.accept("bob", &invite.token, &ok).unwrap();
    let view = joined.view("bob", &|a| a.to_uppercase());
    assert_eq!((view.my_role.as_str(), view.owner_name.as_str(), view.members.len()), ("member", "ALICE", 2));
    assert!(view.invites.is_empty(), "members do not see invites");
    store.accept("bob", &invite.token, &ok).unwrap();
    store.accept("carol", &invite.token, &ok).unwrap();
    assert_eq!(store.accept("dave", &invite.token, &ok).err().unwrap(), Error::Gone);
    assert_eq!(store.list("bob").unwrap().len(), 1);
    assert!(store.list("dave").unwrap().is_empty());
    assert!(store.room("alice", shared.id(), &ok).unwrap().view("alice", &str::to_string).invites.is_empty());
}

#[test]
fn invites_expire_can_be_revoked_and_stop_at_room_deletion() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    let expiring = store.invite("alice", shared.id(), CreateInvite::default(), &ok).unwrap();
    assert_eq!(expiring.expires_at_ms, NOW + INVITE_DEFAULT_SECS * 1000);
    fixture.advance(INVITE_DEFAULT_SECS * 1000);
    assert_eq!(store.accept("bob", &expiring.token, &ok).err().unwrap(), Error::Gone);
    let revoked = store.invite("alice", shared.id(), CreateInvite::default(), &ok).unwrap();
    let after = store.revoke_invite("alice", shared.id(), &revoked.id, &ok).unwrap();
    assert!(after.view("alice", &str::to_string).invites.is_empty());
    assert_eq!(store.accept("bob", &revoked.token, &ok).err().unwrap(), Error::Gone);
    assert_eq!(store.revoke_invite("alice", shared.id(), &revoked.id, &ok).err().unwrap(), Error::Missing);
    for bad in [CreateInvite { expires_in_secs: Some(30), max_uses: None }, CreateInvite { expires_in_secs: None, max_uses: Some(0) },
        CreateInvite { expires_in_secs: Some(INVITE_MAX_SECS + 1), max_uses: None }, CreateInvite { expires_in_secs: None, max_uses: Some(33) }] {
        assert_eq!(store.invite("alice", shared.id(), bad, &ok).err().unwrap(), Error::Invalid);
    }
    join(&mut store, &shared, "bob");
    assert_eq!(store.invite("bob", shared.id(), CreateInvite::default(), &ok).err().unwrap(), Error::Forbidden);
    let late = store.invite("alice", shared.id(), CreateInvite::default(), &ok).unwrap();
    store.delete("alice", shared.id(), &ok).unwrap();
    assert_eq!(store.accept("carol", &late.token, &ok).err().unwrap(), Error::Gone);
    assert!(store.list("bob").unwrap().is_empty());
}

#[test]
fn a_revoked_credential_during_acceptance_commits_no_membership() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    let invite = store.invite("alice", shared.id(), CreateInvite::default(), &ok).unwrap();
    let calls = AtomicUsize::new(0);
    let revoked_late = || calls.fetch_add(1, Ordering::SeqCst) < 2;
    assert_eq!(store.accept("bob", &invite.token, &revoked_late).err().unwrap(), Error::Unauthorized);
    assert!(store.list("bob").unwrap().is_empty());
    assert_eq!(store.room("alice", shared.id(), &ok).unwrap().view("alice", &str::to_string).members.len(), 1);
    store.accept("bob", &invite.token, &ok).unwrap();
}

#[test]
fn only_the_owner_manages_and_members_only_read_or_leave() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    join(&mut store, &shared, "bob");
    join(&mut store, &shared, "carol");
    let session = publish(&mut store, &shared);
    let update = |revision| UpdateRoom { expected_revision: revision, title: Some("Renamed".into()), allow_fork: None };
    let revision = store.room("alice", shared.id(), &ok).unwrap().view("alice", &str::to_string).revision;
    assert_eq!(store.update("bob", shared.id(), update(revision), &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.update("dave", shared.id(), update(revision), &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.delete("bob", shared.id(), &ok).err().unwrap(), Error::Forbidden);
    let bob_desk = Publisher { account: "bob", machine_id: "bob-desk" };
    let request = PublishSession { source_key: "x".into(), title: "x".into(), harness: "codex".into() };
    assert_eq!(store.publish(bob_desk, shared.id(), request, &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.append(bob_desk, shared.id(), &session.id, append(&session, "x\n", false), &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.remove_member("bob", shared.id(), "carol", &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.remove_member("bob", shared.id(), "alice", &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.remove_member("alice", shared.id(), "alice", &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.update("alice", shared.id(), update(revision - 1), &ok).err().unwrap(), Error::Conflict);
    let blank = UpdateRoom { expected_revision: revision, title: Some("  ".into()), allow_fork: None };
    assert_eq!(store.update("alice", shared.id(), blank, &ok).err().unwrap(), Error::Invalid);
    assert_eq!(store.update("alice", shared.id(), update(revision), &ok).unwrap().view("bob", &str::to_string).title, "Renamed");
    let bob_view = store.room("bob", shared.id(), &ok).unwrap().view("bob", &str::to_string);
    assert_eq!((bob_view.sessions[0].source_key.as_str(), bob_view.sessions[0].publisher_machine_id.as_str()), ("", ""));
    assert!(store.remove_member("bob", shared.id(), "bob", &ok).unwrap().is_none());
    assert_eq!(store.read("bob", shared.id(), &session.id, &ReadRange::default(), &ok).err().unwrap(), Error::Missing);
    assert!(store.list("bob").unwrap().is_empty());
    assert!(store.remove_member("alice", shared.id(), "carol", &ok).unwrap().is_some());
    assert_eq!(store.read("carol", shared.id(), &session.id, &ReadRange::default(), &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.remove_member("alice", shared.id(), "carol", &ok).err().unwrap(), Error::Missing);
}

#[test]
fn appends_are_compare_and_swap_and_resets_switch_generation() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    join(&mut store, &shared, "bob");
    let session = publish(&mut store, &shared);
    assert_eq!(publish(&mut store, &shared).id, session.id, "republishing the same source keeps the session");
    let codex = PublishSession { source_key: session.source_key.clone(), title: "x".into(), harness: "codex".into() };
    assert_eq!(store.publish(DESK, shared.id(), codex, &ok).err().unwrap(), Error::Conflict);
    let laptop = Publisher { account: "alice", machine_id: "alice-laptop" };
    assert_eq!(store.append(laptop, shared.id(), &session.id, append(&session, "x\n", false), &ok).err().unwrap(), Error::Forbidden);
    assert_eq!(store.append(DESK, shared.id(), &session.id, append(&session, "no newline", false), &ok).err().unwrap(), Error::Invalid);
    let first = store.append(DESK, shared.id(), &session.id, append(&session, "첫 줄\n", false), &ok).unwrap();
    assert_eq!((first.revision, first.cursor, first.bytes), (1, 10, "첫 줄\n".len() as u64));
    for stale in [append(&session, "x\n", false), SharedAppend { generation: "0".repeat(32), ..append(&first, "x\n", false) },
        SharedAppend { from: 3, next: 13, ..append(&first, "x\n", false) }] {
        assert_eq!(store.append(DESK, shared.id(), &session.id, stale, &ok).err().unwrap(), Error::Conflict);
    }
    let never = || false;
    assert_eq!(store.append(DESK, shared.id(), &session.id, append(&first, "lost\n", false), &never).err().unwrap(), Error::Unauthorized);
    let second = store.append(DESK, shared.id(), &session.id, append(&first, "둘째\n", false), &ok).unwrap();
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "첫 줄\n둘째\n");
    let old = ReadRange { generation: Some(second.generation.clone()), offset: 0, end: Some(second.bytes) };
    let reset = store.append(DESK, shared.id(), &session.id, append(&second, "new\n", true), &ok).unwrap();
    assert_ne!(reset.generation, second.generation);
    assert_eq!(store.read("bob", shared.id(), &session.id, &old, &ok).err().unwrap(), Error::Conflict);
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "new\n");
    let logs = std::fs::read_dir(fixture.room_dir(&shared)).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "log")).count();
    assert_eq!(logs, 1, "the replaced generation is removed");
    let member = store.room("bob", shared.id(), &ok).unwrap().view("bob", &str::to_string);
    assert_eq!((member.sessions[0].cursor, member.sessions[0].bytes), (0, 4));
}

#[test]
fn reads_are_bounded_and_end_on_utf8_boundaries() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    let session = publish(&mut store, &shared);
    let raw = format!("{}\n", "가".repeat(100_000));
    let session = store.append(DESK, shared.id(), &session.id, append(&session, &raw, false), &ok).unwrap();
    let first = store.read("alice", shared.id(), &session.id, &ReadRange::default(), &ok).unwrap();
    assert!(first.raw.len() <= MAX_READ && first.next < first.end);
    assert_eq!(read_all(&store, "alice", &shared, &session.id).unwrap(), raw);
    let middle = ReadRange { generation: None, offset: 1, end: None };
    assert_eq!(store.read("alice", shared.id(), &session.id, &middle, &ok).err().unwrap(), Error::Invalid);
    let past = ReadRange { generation: None, offset: 0, end: Some(session.bytes + 1) };
    assert_eq!(store.read("alice", shared.id(), &session.id, &past, &ok).err().unwrap(), Error::Conflict);
}

#[test]
fn restart_keeps_committed_state_and_hides_uncommitted_tails() {
    let fixture = Fixture::new();
    let (shared, session) = {
        let mut store = fixture.store();
        let shared = room(&mut store, "alice", false);
        join(&mut store, &shared, "bob");
        let session = publish(&mut store, &shared);
        (shared.clone(), store.append(DESK, shared.id(), &session.id, append(&session, "kept\n", false), &ok).unwrap())
    };
    let mut log = std::fs::OpenOptions::new().append(true).open(fixture.content_file(&shared)).unwrap();
    log.write_all(b"torn tail without manifest").unwrap();
    std::fs::create_dir(fixture.root.join("rooms").join(".new-crashed")).unwrap();
    let mut store = fixture.store();
    assert!(!fixture.root.join("rooms").join(".new-crashed").exists());
    assert_eq!(store.list("bob").unwrap().len(), 1);
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "kept\n");
    store.append(DESK, shared.id(), &session.id, append(&session, "next\n", false), &ok).unwrap();
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "kept\nnext\n");
}

#[cfg(unix)]
#[test]
fn damaged_or_redirected_storage_is_refused() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    let session = publish(&mut store, &shared);
    let session = store.append(DESK, shared.id(), &session.id, append(&session, "secret line\n", false), &ok).unwrap();
    let manifest = fixture.room_dir(&shared).join("room.json");
    let good = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"{").unwrap();
    assert_eq!(store.list("alice").err().unwrap(), Error::Storage);
    assert_eq!(store.room("alice", shared.id(), &ok).err().unwrap(), Error::Storage);
    let elsewhere = fixture.root.join("elsewhere.json");
    std::fs::write(&elsewhere, &good).unwrap();
    std::fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &manifest).unwrap();
    assert_eq!(store.room("alice", shared.id(), &ok).err().unwrap(), Error::Storage);
    std::fs::remove_file(&manifest).unwrap();
    std::fs::write(&manifest, &good).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_all(&store, "alice", &shared, &session.id).unwrap(), "secret line\n");
    let log = fixture.content_file(&shared);
    let outside = fixture.root.join("outside.log");
    std::fs::rename(&log, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &log).unwrap();
    assert_eq!(read_all(&store, "alice", &shared, &session.id).err().unwrap(), Error::Storage);
    std::fs::remove_file(&log).unwrap();
    std::fs::rename(&outside, &log).unwrap();
    std::fs::OpenOptions::new().write(true).open(&log).unwrap().set_len(3).unwrap();
    assert_eq!(read_all(&store, "alice", &shared, &session.id).err().unwrap(), Error::Storage);
    std::fs::write(fixture.root.join("accounts").join("alice.json"), b"[]").unwrap();
    assert_eq!(store.list("alice").err().unwrap(), Error::Storage);
}

#[test]
fn checkpoints_are_validated_before_storage() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", true);
    let session = publish(&mut store, &shared);
    let put = |store: &mut Store, value| {
        let request = cp(store, &shared, &session.id, value);
        store.checkpoint(DESK, shared.id(), &session.id, request, &ok)
    };
    let mut wrong = checkpoint("claude", r#"{"type":"user"}"#);
    wrong.format_version = 2;
    assert_eq!(put(&mut store, wrong).err().unwrap(), Error::Invalid);
    let mut tampered = checkpoint("claude", r#"{"type":"user"}"#);
    tampered.native.jsonl = "{\"type\":\"other\"}\n".into();
    assert_eq!(put(&mut store, tampered).err().unwrap(), Error::Invalid);
    let mut future = checkpoint("claude", r#"{"type":"user"}"#);
    future.captured_at_ms = NOW + CLOCK_SKEW_MS_FOR_TESTS + 1;
    assert_eq!(put(&mut store, future).err().unwrap(), Error::Invalid);
    assert_eq!(put(&mut store, checkpoint("claude", "not json")).err().unwrap(), Error::Invalid);
    assert_eq!(put(&mut store, checkpoint("codex", r#"{"type":"user"}"#)).err().unwrap(), Error::Invalid);
    let mut huge = checkpoint("claude", &format!(r#"{{"text":"{}"}}"#, "x".repeat(16 << 20)));
    huge.native.sha256 = digest(huge.native.jsonl.as_bytes());
    assert_eq!(put(&mut store, huge).err().unwrap(), Error::Limit);
    for path in ["../escape", "/etc/passwd", ".git/config", "src/.env", "src\\main.rs", "keys/server.pem", "C:/x", "a//b", ".ssh/config"] {
        assert!(!installable_project_path(path), "{path}");
        assert_eq!(put(&mut store, with_project(vec![file(path, b"x")], "git@github.com:team/app.git")).err().unwrap(), Error::Invalid, "{path}");
    }
    let mut mismatched = file("src/main.rs", b"fn main() {}");
    mismatched.sha256 = digest(b"other");
    assert_eq!(put(&mut store, with_project(vec![mismatched], "git@github.com:team/app.git")).err().unwrap(), Error::Invalid);
    assert_eq!(put(&mut store, with_project(vec![], "https://user:token@github.com/team/app.git")).err().unwrap(), Error::Invalid);
    let twice = vec![file("src/main.rs", b"a"), file("SRC/main.rs", b"b")];
    assert_eq!(put(&mut store, with_project(twice, "git@github.com:team/app.git")).err().unwrap(), Error::Invalid);
    let summary = put(&mut store, with_project(vec![file("src/main.rs", b"fn main() {}")], "https://github.com/team/app.git")).unwrap();
    assert!(summary.has_project);
    let other = PublishSession { source_key: "codex".into(), title: "x".into(), harness: "codex".into() };
    let codex = store.publish(DESK, shared.id(), other, &ok).unwrap();
    assert_eq!(store.checkpoint(DESK, shared.id(), &codex.id, cp(&store, &shared, &codex.id, checkpoint("codex", r#"{"type":"session_meta"}"#)), &ok).unwrap().harness, "codex");
}

const CLOCK_SKEW_MS_FOR_TESTS: u64 = 10 * 60 * 1000;

#[test]
fn forks_are_idempotent_per_recipient_and_independent_of_the_source() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    join(&mut store, &shared, "bob");
    join(&mut store, &shared, "carol");
    let session = publish(&mut store, &shared);
    let session = store.append(DESK, shared.id(), &session.id, append(&session, "before\n", false), &ok).unwrap();
    let laptop = Publisher { account: "alice", machine_id: "alice-laptop" };
    let first = checkpoint("claude", r#"{"uuid":"first"}"#);
    assert_eq!(store.checkpoint(laptop, shared.id(), &session.id, cp(&store, &shared, &session.id, first.clone()), &ok).err().unwrap(), Error::Forbidden);
    let bob_desk = Publisher { account: "bob", machine_id: "bob-desk" };
    assert_eq!(store.checkpoint(bob_desk, shared.id(), &session.id, cp(&store, &shared, &session.id, first.clone()), &ok).err().unwrap(), Error::Forbidden);
    let summary = store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, first), &ok).unwrap();
    let request = |id: &str| CreateFork { checkpoint_id: summary.id.clone(), request_id: id.into() };
    assert_eq!(store.fork("bob", shared.id(), &session.id, request("request-0001"), &ok).err().unwrap(), Error::ForkDisabled);
    let revision = store.room("alice", shared.id(), &ok).unwrap().view("alice", &str::to_string).revision;
    store.update("alice", shared.id(), UpdateRoom { expected_revision: revision, title: None, allow_fork: Some(true) }, &ok).unwrap();
    let unknown = CreateFork { checkpoint_id: "f".repeat(32), request_id: "request-0001".into() };
    assert_eq!(store.fork("bob", shared.id(), &session.id, unknown, &ok).err().unwrap(), Error::Conflict);
    assert_eq!(store.fork("dave", shared.id(), &session.id, request("request-0001"), &ok).err().unwrap(), Error::Missing);
    let bob = store.fork("bob", shared.id(), &session.id, request("request-0001"), &ok).unwrap();
    assert_eq!(store.fork("bob", shared.id(), &session.id, request("request-0001"), &ok).unwrap().id, bob.id);
    let carol = store.fork("carol", shared.id(), &session.id, request("request-0001"), &ok).unwrap();
    assert_ne!(carol.id, bob.id);
    let listed = |store: &Store, account| store.forks(account, &ok).unwrap().into_iter().map(|f| f.id).collect::<Vec<_>>();
    assert_eq!((listed(&store, "bob"), listed(&store, "carol")), (vec![bob.id.clone()], vec![carol.id.clone()]));
    assert!(listed(&store, "alice").is_empty());
    let info = fixture.root.join("forks").join("bob").join(format!("{}.info", bob.id));
    std::fs::remove_file(&info).unwrap();
    assert_eq!(listed(&store, "bob"), vec![bob.id.clone()], "the listing cache is rebuilt from the copy");
    assert!(info.exists());
    std::fs::write(&info, b"{").unwrap();
    assert_eq!(listed(&store, "bob"), vec![bob.id.clone()]);
    assert_eq!(store.fork("bob", shared.id(), &session.id, request("request-0001"), &ok).unwrap().id, bob.id);
    let second = later(checkpoint("claude", r#"{"uuid":"second"}"#), 1);
    let replaced = store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, second), &ok).unwrap();
    let moved = CreateFork { checkpoint_id: replaced.id.clone(), request_id: "request-0001".into() };
    assert_eq!(store.fork("bob", shared.id(), &session.id, moved, &ok).err().unwrap(), Error::Conflict);
    assert_eq!(store.fork("bob", shared.id(), &session.id, request("request-0002"), &ok).err().unwrap(), Error::Conflict);
    store.append(DESK, shared.id(), &session.id, append(&session, "after\n", false), &ok).unwrap();
    store.delete("alice", shared.id(), &ok).unwrap();
    assert_eq!(listed(&store, "bob"), vec![bob.id.clone()]);
    let copy = store.fork_copy("bob", &bob.id, &ok).unwrap();
    assert_eq!((copy.checkpoint.native.jsonl.as_str(), copy.info.checkpoint_id.as_str()), ("{\"uuid\":\"first\"}\n", summary.id.as_str()));
    assert_eq!(store.fork("bob", shared.id(), &session.id, request("request-0001"), &ok).unwrap().id, bob.id);
    assert_eq!(store.fork_copy("carol", &bob.id, &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.fork_copy("alice", &bob.id, &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.delete_fork("carol", &bob.id, &ok).err().unwrap(), Error::Missing);
    store.delete_fork("bob", &bob.id, &ok).unwrap();
    assert_eq!(store.fork_copy("bob", &bob.id, &ok).err().unwrap(), Error::Missing);
    assert!(listed(&store, "bob").is_empty());
    assert!(store.fork_copy("carol", &carol.id, &ok).is_ok());
}

#[test]
fn limits_bound_rooms_sessions_members_and_bytes() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let rooms: Vec<_> = (0..MAX_OWNED).map(|_| room(&mut store, "alice", false)).collect();
    let more = CreateRoom { title: "one more".into(), allow_fork: false, request_id: Some("form-0000-0001".into()) };
    assert_eq!(store.create("alice", more, &ok).err().unwrap(), Error::Limit);
    let shared = &rooms[0];
    let invite = store.invite("alice", shared.id(), CreateInvite { expires_in_secs: None, max_uses: Some(32) }, &ok).unwrap();
    for n in 1..MAX_MEMBERS { store.accept(&format!("m{n:02}"), &invite.token, &ok).unwrap(); }
    assert_eq!(store.accept("late", &invite.token, &ok).err().unwrap(), Error::Limit);
    assert_eq!(store.invite("alice", shared.id(), CreateInvite::default(), &ok).err().unwrap(), Error::Limit);
    for n in 0..MAX_SESSIONS {
        let request = PublishSession { source_key: format!("s{n}"), title: "t".into(), harness: "claude".into() };
        store.publish(DESK, rooms[1].id(), request, &ok).unwrap();
    }
    let extra = PublishSession { source_key: "extra".into(), title: "t".into(), harness: "claude".into() };
    assert_eq!(store.publish(DESK, rooms[1].id(), extra, &ok).err().unwrap(), Error::Limit);
    let mut session = publish(&mut store, &rooms[2]);
    let chunk = format!("{}\n", "x".repeat(MAX_APPEND - 1));
    for _ in 0..(MAX_SESSION_BYTES as usize / MAX_APPEND) {
        session = store.append(DESK, rooms[2].id(), &session.id, append(&session, &chunk, false), &ok).unwrap();
    }
    assert_eq!(store.append(DESK, rooms[2].id(), &session.id, append(&session, "x\n", false), &ok).err().unwrap(), Error::Limit);
    let oversized = format!("{}\n", "x".repeat(MAX_APPEND));
    assert_eq!(store.append(DESK, rooms[2].id(), &session.id, append(&session, &oversized, false), &ok).err().unwrap(), Error::Invalid);
}

#[test]
fn creation_retries_with_a_request_id_return_the_same_room() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let request = |title: &str, allow_fork| CreateRoom { title: title.into(), allow_fork, request_id: Some("form-7f3a-0001".into()) };
    let first = store.create("alice", request("Pairing", false), &ok).unwrap();
    assert_eq!(store.create("alice", request(" Pairing ", false), &ok).unwrap().id(), first.id());
    let revision = store.room("alice", first.id(), &ok).unwrap().view("alice", &str::to_string).revision;
    store.update("alice", first.id(), UpdateRoom { expected_revision: revision, title: Some("Renamed".into()), allow_fork: None }, &ok).unwrap();
    assert_eq!(store.create("alice", request("Pairing", false), &ok).unwrap().id(), first.id(), "a rename does not break the retry");
    assert_eq!(store.create("alice", request("Pairing", true), &ok).err().unwrap(), Error::Conflict);
    assert_eq!(store.create("alice", request("Other", false), &ok).err().unwrap(), Error::Conflict);
    assert_eq!(store.list("alice").unwrap().len(), 1);
    assert_ne!(store.create("bob", request("Pairing", false), &ok).unwrap().id(), first.id());
    let short = CreateRoom { title: "Pairing".into(), allow_fork: false, request_id: Some("short".into()) };
    assert_eq!(store.create("alice", short, &ok).err().unwrap(), Error::Invalid);
    drop(store);
    assert_eq!(fixture.store().create("alice", request("Pairing", false), &ok).unwrap().id(), first.id(), "survives a restart");
}

#[test]
fn unpublishing_one_session_ends_its_reads_and_keeps_taken_forks() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", true);
    join(&mut store, &shared, "bob");
    let session = publish(&mut store, &shared);
    let session = store.append(DESK, shared.id(), &session.id, append(&session, "shared\n", false), &ok).unwrap();
    let other = PublishSession { source_key: "other".into(), title: "Other".into(), harness: "claude".into() };
    let other = store.publish(DESK, shared.id(), other, &ok).unwrap();
    let first = store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, checkpoint("claude", r#"{"uuid":"a"}"#)), &ok).unwrap();
    let resent = store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, checkpoint("claude", r#"{"uuid":"a"}"#)), &ok).unwrap();
    assert_eq!(resent.id, first.id, "the same content keeps its checkpoint id");
    let fork = store.fork("bob", shared.id(), &session.id, CreateFork { checkpoint_id: first.id.clone(), request_id: "request-0001".into() }, &ok).unwrap();
    assert_eq!(store.unpublish("bob", shared.id(), &session.id, &ok).err().unwrap(), Error::Forbidden);
    store.unpublish("alice", shared.id(), &session.id, &ok).unwrap();
    assert_eq!(store.unpublish("alice", shared.id(), &session.id, &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.read("bob", shared.id(), &session.id, &ReadRange::default(), &ok).err().unwrap(), Error::Missing);
    assert_eq!(store.append(DESK, shared.id(), &session.id, append(&session, "late\n", false), &ok).err().unwrap(), Error::Missing);
    let left: Vec<_> = std::fs::read_dir(fixture.room_dir(&shared)).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with(&session.id) || name.ends_with(".checkpoint")).collect();
    assert!(left.is_empty(), "{left:?}");
    let view = store.room("bob", shared.id(), &ok).unwrap().view("bob", &str::to_string);
    assert_eq!(view.sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec![other.id.as_str()]);
    assert_eq!(store.fork_copy("bob", &fork.id, &ok).unwrap().checkpoint.native.jsonl, "{\"uuid\":\"a\"}\n");
    let again = publish(&mut store, &shared);
    assert_eq!((again.id.as_str(), again.bytes, again.revision), (session.id.as_str(), 0, 0));
    assert_ne!(again.generation, session.generation);
}

fn first_call_only() -> impl Fn() -> bool {
    let calls = AtomicUsize::new(0);
    move || calls.fetch_add(1, Ordering::SeqCst) == 0
}

fn lapses_on_third_call() -> impl Fn() -> bool {
    let calls = AtomicUsize::new(0);
    move || calls.fetch_add(1, Ordering::SeqCst) < 2
}

fn named(fixture: &Fixture, room: &Room, suffix: &str) -> Vec<(String, u64)> {
    let mut found: Vec<_> = std::fs::read_dir(fixture.room_dir(room)).unwrap().map(|e| e.unwrap())
        .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
        .map(|e| (e.file_name().into_string().unwrap(), e.metadata().unwrap().len())).collect();
    found.sort();
    found
}

#[test]
fn failed_writes_and_crashes_leave_nothing_outside_the_manifest() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", true);
    join(&mut store, &shared, "bob");
    let session = publish(&mut store, &shared);
    let kept = store.append(DESK, shared.id(), &session.id, append(&session, "kept\n", false), &ok).unwrap();
    let current = format!("{}-{}.log", kept.id, kept.generation);
    assert_eq!(store.append(DESK, shared.id(), &session.id, append(&kept, "replacement\n", true), &lapses_on_third_call()).err().unwrap(), Error::Unauthorized);
    assert_eq!(named(&fixture, &shared, ".log"), vec![(current.clone(), 5)], "the uncommitted generation is reclaimed");
    assert_eq!(store.append(DESK, shared.id(), &session.id, append(&kept, "tail\n", false), &lapses_on_third_call()).err().unwrap(), Error::Unauthorized);
    assert_eq!(named(&fixture, &shared, ".log"), vec![(current.clone(), 5)], "the uncommitted tail is cut");
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "kept\n");
    let first = store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, checkpoint("claude", r#"{"uuid":"a"}"#)), &ok).unwrap();
    let newer = cp(&store, &shared, &session.id, later(checkpoint("claude", r#"{"uuid":"b"}"#), 1));
    let refused = store.checkpoint(DESK, shared.id(), &session.id, newer, &lapses_on_third_call());
    assert_eq!(refused.err().unwrap(), Error::Unauthorized);
    let first_file = format!("{}.checkpoint", first.id);
    assert_eq!(named(&fixture, &shared, ".checkpoint").into_iter().map(|f| f.0).collect::<Vec<_>>(), vec![first_file.clone()]);
    let dir = fixture.room_dir(&shared);
    std::fs::write(dir.join(format!("{}-{}.log", kept.id, "f".repeat(32))), b"orphan generation\n").unwrap();
    std::fs::write(dir.join(format!("{}.checkpoint", "e".repeat(32))), b"{}").unwrap();
    std::fs::write(dir.join(".room.json-crashed.tmp"), b"partial").unwrap();
    std::fs::OpenOptions::new().append(true).open(dir.join(&current)).unwrap().write_all(b"torn").unwrap();
    drop(store);
    let mut store = fixture.store();
    assert_eq!(named(&fixture, &shared, ".log"), vec![(current, 5)]);
    assert_eq!(named(&fixture, &shared, ".checkpoint").into_iter().map(|f| f.0).collect::<Vec<_>>(), vec![first_file]);
    assert!(named(&fixture, &shared, ".tmp").is_empty());
    assert_eq!(read_all(&store, "bob", &shared, &session.id).unwrap(), "kept\n");
    let fork = CreateFork { checkpoint_id: first.id.clone(), request_id: "request-0001".into() };
    assert_eq!(store.fork("bob", shared.id(), &session.id, fork, &ok).map(|f| f.checkpoint_id).unwrap(), first.id);
}

#[test]
fn early_returns_check_the_credential_once_more() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", true);
    let invite = store.invite("alice", shared.id(), CreateInvite { expires_in_secs: None, max_uses: Some(2) }, &ok).unwrap();
    store.accept("bob", &invite.token, &ok).unwrap();
    assert_eq!(store.accept("bob", &invite.token, &first_call_only()).err().unwrap(), Error::Unauthorized);
    let session = publish(&mut store, &shared);
    let same = PublishSession { source_key: session.source_key.clone(), title: session.title.clone(), harness: "claude".into() };
    assert_eq!(store.publish(DESK, shared.id(), same, &first_call_only()).err().unwrap(), Error::Unauthorized);
    store.checkpoint(DESK, shared.id(), &session.id, cp(&store, &shared, &session.id, checkpoint("claude", r#"{"uuid":"a"}"#)), &ok).unwrap();
    let request = cp(&store, &shared, &session.id, checkpoint("claude", r#"{"uuid":"a"}"#));
    let resent = store.checkpoint(DESK, shared.id(), &session.id, request, &first_call_only());
    assert_eq!(resent.err().unwrap(), Error::Unauthorized);
}

#[test]
fn removed_members_need_an_invite_issued_after_the_removal() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", false);
    let old = store.invite("alice", shared.id(), CreateInvite { expires_in_secs: None, max_uses: Some(5) }, &ok).unwrap();
    store.accept("bob", &old.token, &ok).unwrap();
    store.accept("carol", &old.token, &ok).unwrap();
    store.remove_member("alice", shared.id(), "bob", &ok).unwrap();
    assert_eq!(store.accept("bob", &old.token, &ok).err().unwrap(), Error::Gone);
    drop(store);
    let mut store = fixture.store();
    assert_eq!(store.accept("bob", &old.token, &ok).err().unwrap(), Error::Gone, "the removal survives a restart");
    store.remove_member("carol", shared.id(), "carol", &ok).unwrap();
    store.accept("carol", &old.token, &ok).unwrap();
    store.accept("dave", &old.token, &ok).unwrap();
    let fresh = store.invite("alice", shared.id(), CreateInvite::default(), &ok).unwrap();
    store.accept("bob", &fresh.token, &ok).unwrap();
    assert_eq!(store.list("bob").unwrap().len(), 1);
}

#[test]
fn a_deleted_room_is_not_recreated_by_a_late_retry() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let request = || CreateRoom { title: "Pairing".into(), allow_fork: false, request_id: Some("form-dead-0001".into()) };
    let first = store.create("alice", request(), &ok).unwrap();
    store.delete("alice", first.id(), &ok).unwrap();
    assert_eq!(store.create("alice", request(), &ok).err().unwrap(), Error::Conflict);
    drop(store);
    let mut store = fixture.store();
    assert_eq!(store.create("alice", request(), &ok).err().unwrap(), Error::Conflict);
    assert!(store.list("alice").unwrap().is_empty());
    let other = CreateRoom { request_id: Some("form-live-0002".into()), ..request() };
    assert_ne!(store.create("alice", other, &ok).unwrap().id(), first.id());
    fixture.advance(recovery::TOMBSTONE_RETENTION_MS + 1);
    drop(store);
    let mut store = fixture.store();
    assert_eq!(store.create("alice", request(), &ok).unwrap().id(), first.id(), "only an expired tombstone lets the id return");
}

#[test]
fn checkpoints_only_move_forward_and_retire_with_their_generation() {
    let fixture = Fixture::new();
    let mut store = fixture.store();
    let shared = room(&mut store, "alice", true);
    join(&mut store, &shared, "bob");
    let session = publish(&mut store, &shared);
    let (id, sid) = (shared.id().to_string(), session.id.clone());
    let s1 = store.append(DESK, &id, &sid, append(&session, "one\n", false), &ok).unwrap();
    let at = |s: &SharedSession, revision: u64, checkpoint: ForkCheckpoint| PublishCheckpoint {
        expected_generation: s.generation.clone(), expected_revision: revision, checkpoint,
    };
    let slow = checkpoint("claude", r#"{"uuid":"slow"}"#);
    let fresh = later(checkpoint("claude", r#"{"uuid":"fresh"}"#), 500);
    let newest = store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision, fresh.clone()), &ok).unwrap();
    assert_eq!(store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision, slow.clone()), &ok).err().unwrap(), Error::Conflict, "a slower capture loses");
    assert_eq!(store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision - 1, later(slow.clone(), 900)), &ok).err().unwrap(), Error::Conflict);
    assert_eq!(store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision + 1, later(slow.clone(), 900)), &ok).err().unwrap(), Error::Conflict, "not ahead of the session");
    let s2 = store.append(DESK, &id, &sid, append(&s1, "two\n", false), &ok).unwrap();
    assert_eq!(store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision, fresh.clone()), &ok).unwrap().id, newest.id, "a resend keeps its id");
    let s3 = store.append(DESK, &id, &sid, append(&s2, "reset\n", true), &ok).unwrap();
    assert!(store.room("bob", &id, &ok).unwrap().view("bob", &str::to_string).sessions[0].checkpoint.is_none());
    assert_eq!(store.checkpoint(DESK, &id, &sid, at(&s1, s1.revision, fresh.clone()), &ok).err().unwrap(), Error::Conflict, "the old generation stays retired");
    let fork = CreateFork { checkpoint_id: newest.id.clone(), request_id: "request-0001".into() };
    assert_eq!(store.fork("bob", &id, &sid, fork, &ok).err().unwrap(), Error::Missing);
    let reborn = store.checkpoint(DESK, &id, &sid, at(&s3, s3.revision, fresh), &ok).unwrap();
    assert_ne!(reborn.id, newest.id);
    assert_eq!(named(&fixture, &shared, ".checkpoint").into_iter().map(|f| f.0).collect::<Vec<_>>(), vec![format!("{}.checkpoint", reborn.id)]);
}
