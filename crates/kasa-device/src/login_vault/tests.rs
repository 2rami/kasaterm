//! 두 기기와 시험용 관문으로 한 바퀴씩 — 진짜 보관 코드를 HTTP 없이 부른다.

use std::cell::Cell;

use super::crypto::VaultKey;
use super::engine::{Counts, Engine, Remote, RemoteError, SyncError};
use super::record::{MAX_SKEW_MS, TOMBSTONE_KEEP_MS};
use super::server::{self, put_message, Fetched, Put, PutOutcome};
use super::state::State;
use super::store::{LoginStore, MemoryStore};

const ACCOUNT: &str = "acct";

/// 시험용 관문. 관문 시계는 기기 시계와 따로 간다.
struct Relay {
    store: server::Store,
    now: Cell<u64>,
    /// 악의적인 관문 흉내 — 받기 응답의 차례와 시계를 이 값으로 속인다.
    forged: std::cell::RefCell<Option<(u64, u64)>>,
}

impl Relay {
    fn new() -> Self {
        Self { store: server::Store::new(None), now: Cell::new(1_000_000), forged: Default::default() }
    }

    /// 악의적인 관문 흉내 — 예전에 받은 덩어리를 그 항목의 새 차례로 다시 놓는다. 서명 검사는 관문 자신의 몫이라,
    /// 시험은 열쇠로 서명해 그 검사를 지나게 한다.
    fn replay(&self, key: &VaultKey, id: &str, blob: &str) {
        let current = self.store.fetch(ACCOUNT, 0, 0).unwrap().items.into_iter().find(|i| i.id == id).map_or(0, |i| i.rev);
        let puts = [Put { id: id.into(), expected_rev: current, blob: Some(blob.into()) }];
        let sig = key.sign(&put_message(ACCOUNT, &key.key_id(), &puts));
        self.store.put(ACCOUNT, &key.key_id(), &puts, &sig, self.now.get()).unwrap();
    }

    fn blob(&self, id: &str) -> String {
        self.store.fetch(ACCOUNT, 0, 0).unwrap().items.into_iter().find(|i| i.id == id).unwrap().blob
    }
}

impl Remote for Relay {
    fn init(&self, key_id: &str, sign_pub: &str) -> Result<(), RemoteError> {
        self.store.init(ACCOUNT, key_id, sign_pub, "test", self.now.get()).map_err(|e| match e {
            server::Error::Exists(k) => RemoteError::Exists(k),
            _ => RemoteError::Failed,
        })
    }
    fn fetch(&self, since: u64) -> Result<Fetched, RemoteError> {
        if let Some((revision, now)) = *self.forged.borrow() {
            // 차례를 되돌린 척 — 가진 덩어리를 전부 차례 1 로 다시 내민다.
            let mut all = self.store.fetch(ACCOUNT, 0, now).map_err(|_| RemoteError::Failed)?;
            all.revision = revision;
            all.now_ms = now;
            for item in &mut all.items {
                item.rev = 1;
            }
            return Ok(all);
        }
        self.store.fetch(ACCOUNT, since, self.now.get()).map_err(|_| RemoteError::Failed)
    }
    fn put(&self, key_id: &str, puts: &[Put], sig: &str) -> Result<Vec<PutOutcome>, RemoteError> {
        self.store.put(ACCOUNT, key_id, puts, sig, self.now.get()).map_err(|e| match e {
            server::Error::KeyMismatch => RemoteError::KeyMismatch,
            server::Error::BadSignature => RemoteError::BadSignature,
            server::Error::NotFound => RemoteError::NotFound,
            _ => RemoteError::Failed,
        })
    }
}

struct Device {
    name: &'static str,
    key: VaultKey,
    store: MemoryStore,
    state: State,
}

impl Device {
    fn new(name: &'static str, key: &VaultKey) -> Self {
        Self { name, key: VaultKey::from_base64(&key.to_base64()).unwrap(), store: MemoryStore::default(), state: State::default() }
    }

    /// 이 기기 시계로 한 바퀴. 관문 시계는 `relay.now` 그대로다.
    fn sync_at(&mut self, relay: &Relay, now: u64, allow_bulk_delete: bool) -> Result<Counts, SyncError> {
        let engine = Engine { account: ACCOUNT, device: self.name, key: &self.key, store: &self.store, remote: relay, now_ms: now, allow_bulk_delete };
        engine.round(&mut self.state)
    }

    /// 기기 시계와 관문 시계가 같은 한 바퀴.
    fn sync(&mut self, relay: &Relay, now: u64) -> Result<Counts, SyncError> {
        relay.now.set(now);
        self.sync_at(relay, now, false)
    }

    fn edit(&self, at: u64, host: &str, user: &str, password: &str) {
        self.store.set_clock(at - 1);
        self.store.write(host, user, password).unwrap();
    }

    fn password(&self, host: &str, user: &str) -> Option<String> {
        self.store.read(host, user).unwrap().map(|p| p.to_string())
    }
}

fn vault() -> (Relay, VaultKey) {
    let relay = Relay::new();
    let key = VaultKey::generate().unwrap();
    relay.init(&key.key_id(), &key.sign_pub()).unwrap();
    (relay, key)
}

#[test]
fn a_second_device_receives_everything_and_the_relay_sees_no_plaintext() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    a.edit(1_000, "mail.example", "me@example", "hunter2");
    a.edit(1_001, "shop.example", "Buyer", "s3cret");
    assert_eq!(a.sync(&relay, 2_000).unwrap().uploaded, 2);
    let raw = serde_json::to_string(&relay.store.fetch(ACCOUNT, 0, 0).unwrap()).unwrap();
    for plain in ["mail.example", "me@example", "hunter2", "shop.example", "Buyer", "s3cret"] {
        assert!(!raw.contains(plain), "관문 사본에 평문이 보이면 안 된다");
    }
    let state = serde_json::to_string(&a.state).unwrap();
    assert!(!state.contains("hunter2") && !state.contains("mail.example"), "로컬 상태에도 평문이 없다");

    let mut b = Device::new("b", &key);
    assert_eq!(b.sync(&relay, 3_000).unwrap().received, 2);
    assert_eq!(b.store.snapshot(), a.store.snapshot());
    assert_eq!(b.sync(&relay, 4_000).unwrap(), Counts::default(), "맞춘 뒤 한 바퀴는 아무것도 안 한다");
    assert_eq!(a.sync(&relay, 4_000).unwrap(), Counts::default(), "제가 올린 것을 되받지 않는다");
}

#[test]
fn the_later_change_wins_whichever_device_uploads_first() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "x.example", "u", "v1");
    a.sync(&relay, 1_500).unwrap();
    b.sync(&relay, 1_600).unwrap();

    a.edit(2_000, "x.example", "u", "from-a");
    b.edit(3_000, "x.example", "u", "from-b");
    a.sync(&relay, 4_000).unwrap();
    b.sync(&relay, 4_100).unwrap();
    a.sync(&relay, 4_200).unwrap();
    assert_eq!(a.password("x.example", "u").as_deref(), Some("from-b"));
    assert_eq!(b.password("x.example", "u").as_deref(), Some("from-b"));

    b.edit(5_000, "x.example", "u", "older-b");
    a.edit(6_000, "x.example", "u", "newer-a");
    b.sync(&relay, 7_000).unwrap();
    a.sync(&relay, 7_100).unwrap();
    b.sync(&relay, 7_200).unwrap();
    assert_eq!(a.password("x.example", "u").as_deref(), Some("newer-a"));
    assert_eq!(b.password("x.example", "u").as_deref(), Some("newer-a"));
}

#[test]
fn a_replayed_old_blob_or_tombstone_is_refused_and_the_newer_one_restored() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "x.example", "u", "old");
    a.sync(&relay, 1_500).unwrap();
    let id = key.item_id("x.example", "u");
    let old_blob = relay.blob(&id);
    a.edit(2_000, "x.example", "u", "new");
    a.sync(&relay, 2_500).unwrap();
    b.sync(&relay, 2_600).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("new"));

    relay.replay(&key, &id, &old_blob);
    b.sync(&relay, 3_000).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("new"), "옛 덩어리가 새 차례로 와도 되돌리지 않는다");
    let mut c = Device::new("c", &key);
    c.sync(&relay, 3_100).unwrap();
    assert_eq!(c.password("x.example", "u").as_deref(), Some("new"), "아는 판을 다시 올려 관문도 새 값으로 돌아왔다");

    // 옛 묘비로 지금 비밀번호를 지우지 못한다.
    a.store.delete("x.example", "u").unwrap();
    a.sync(&relay, 4_000).unwrap();
    let tombstone = relay.blob(&id);
    a.edit(5_000, "x.example", "u", "revived");
    a.sync(&relay, 5_500).unwrap();
    b.sync(&relay, 5_600).unwrap();
    relay.replay(&key, &id, &tombstone);
    b.sync(&relay, 6_000).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("revived"));
}

#[test]
fn deletions_travel_as_tombstones_unless_the_item_changed_later() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "gone.example", "u", "p");
    a.edit(1_001, "kept.example", "u", "p");
    a.sync(&relay, 2_000).unwrap();
    b.sync(&relay, 2_100).unwrap();

    a.store.delete("gone.example", "u").unwrap();
    assert_eq!(a.sync(&relay, 3_000).unwrap().uploaded, 1);
    assert_eq!(b.sync(&relay, 3_100).unwrap().tombstones, 1);
    assert_eq!(b.password("gone.example", "u"), None);
    assert!(b.password("kept.example", "u").is_some());

    a.store.delete("kept.example", "u").unwrap();
    b.edit(9_000, "kept.example", "u", "edited-later");
    a.sync(&relay, 5_000).unwrap();
    b.sync(&relay, 9_500).unwrap();
    a.sync(&relay, 9_600).unwrap();
    assert_eq!(a.password("kept.example", "u").as_deref(), Some("edited-later"));
}

#[test]
fn a_list_that_suddenly_comes_back_empty_does_not_delete_everywhere() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    for i in 0..20 {
        a.edit(1_000 + i, &format!("s{i}.example"), "u", "p");
    }
    a.sync(&relay, 2_000).unwrap();
    b.sync(&relay, 2_100).unwrap();
    for i in 0..20 {
        a.store.delete(&format!("s{i}.example"), "u").unwrap();
    }
    let counts = a.sync(&relay, 3_000).unwrap();
    assert_eq!((counts.deletions_held, counts.uploaded), (20, 0), "한도를 넘는 지우기는 사람 확인을 기다린다");
    b.sync(&relay, 3_100).unwrap();
    assert_eq!(b.store.snapshot().len(), 20, "다른 기기는 그대로");
    relay.now.set(3_200);
    assert_eq!(a.sync_at(&relay, 3_200, true).unwrap().uploaded, 20, "사람이 확인하면 그때 올린다");
    b.sync(&relay, 3_300).unwrap();
    assert!(b.store.snapshot().is_empty());

    // 몇 개뿐인 지우기는 묻지 않는다.
    a.edit(4_000, "one.example", "u", "p");
    a.sync(&relay, 4_100).unwrap();
    a.store.delete("one.example", "u").unwrap();
    assert_eq!(a.sync(&relay, 4_200).unwrap().uploaded, 1);
}

#[test]
fn a_state_file_copied_from_another_device_is_rebuilt_not_trusted() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    a.edit(1_000, "x.example", "u", "p");
    a.edit(1_001, "y.example", "u", "p");
    a.sync(&relay, 2_000).unwrap();
    let mut b = Device::new("b", &key);
    b.state = a.state.clone();
    let counts = b.sync(&relay, 3_000).unwrap();
    assert_eq!((counts.uploaded, counts.received), (0, 2), "남의 상태로 지운 것을 만들지 않고 처음부터 받는다");
    let mut c = Device::new("c", &key);
    c.sync(&relay, 3_100).unwrap();
    assert_eq!(c.store.snapshot().len(), 2);
}

#[test]
fn a_failed_write_is_retried_unless_the_person_changed_it_meanwhile() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "x.example", "u", "v1");
    a.sync(&relay, 1_500).unwrap();
    b.sync(&relay, 1_600).unwrap();

    a.edit(2_000, "x.example", "u", "from-a");
    a.sync(&relay, 2_500).unwrap();
    b.store.fail_writes(true);
    assert_eq!(b.sync(&relay, 2_600).unwrap().failed, 1);
    b.store.fail_writes(false);
    b.edit(3_000, "x.example", "u", "person-on-b");
    b.sync(&relay, 3_500).unwrap();
    a.sync(&relay, 3_600).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("person-on-b"), "재시도가 사람이 고친 값을 덮지 않는다");
    assert_eq!(a.password("x.example", "u").as_deref(), Some("person-on-b"));

    a.edit(4_000, "x.example", "u", "again-a");
    a.sync(&relay, 4_100).unwrap();
    b.store.fail_writes(true);
    b.sync(&relay, 4_200).unwrap();
    b.store.fail_writes(false);
    b.sync(&relay, 4_300).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("again-a"), "그사이 안 고쳤으면 다시 쓴다");
}

#[test]
fn a_tombstone_the_keychain_refuses_is_held_not_resurrected() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "owned.example", "u", "p");
    a.sync(&relay, 2_000).unwrap();
    b.sync(&relay, 2_100).unwrap();
    b.store.refuse_delete_for("owned.example");

    a.store.delete("owned.example", "u").unwrap();
    a.sync(&relay, 3_000).unwrap();
    assert_eq!(b.sync(&relay, 3_100).unwrap().held, 1);
    assert!(b.password("owned.example", "u").is_some(), "키체인이 거절하면 남는다");
    let again = b.sync(&relay, 3_200).unwrap();
    assert_eq!((again.held, again.uploaded), (1, 0), "남은 값을 새 변경으로 착각해 되살리지 않는다");

    b.edit(4_000, "owned.example", "u", "person-edited");
    let counts = b.sync(&relay, 4_100).unwrap();
    assert_eq!((counts.uploaded, counts.held), (1, 0), "사람이 고치면 그때는 살아 있는 항목이다");
    a.sync(&relay, 4_200).unwrap();
    assert_eq!(a.password("owned.example", "u").as_deref(), Some("person-edited"));
}

#[test]
fn a_clock_far_ahead_stops_instead_of_winning() {
    let (relay, key) = vault();
    let mut fast = Device::new("fast", &key);
    let mut b = Device::new("b", &key);
    let real = 10_000_000;
    b.edit(real - 1_000, "x.example", "u", "honest");
    b.sync(&relay, real).unwrap();
    fast.edit(real + 3_600_000, "x.example", "u", "from-fast-clock");
    relay.now.set(real);
    assert_eq!(fast.sync_at(&relay, real + 3_600_000, false), Err(SyncError::ClockSkew), "한 시간 앞선 기기는 그 바퀴를 멈춘다");
    b.sync(&relay, real + 1_000).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("honest"));
    // 5분 안의 차이는 받는다 — 그 안에서는 깎아 봉인한다.
    relay.now.set(real + 2_000);
    fast.store.set_clock(real + 2_000 + MAX_SKEW_MS - 10);
    fast.store.write("x.example", "u", "slightly-ahead").unwrap();
    assert!(fast.sync_at(&relay, real + 2_000 + MAX_SKEW_MS - 1, false).is_ok());
}

#[test]
fn a_relay_that_rolls_back_its_revision_and_clock_cannot_revive_old_values() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    let real = 10_000_000;
    a.edit(real - 2_000, "x.example", "u", "old");
    a.edit(real - 1_999, "y.example", "u", "keep");
    a.sync(&relay, real).unwrap();
    a.edit(real + 1_000, "x.example", "u", "new");
    a.store.delete("y.example", "u").unwrap();
    a.sync(&relay, real + 2_000).unwrap();
    b.sync(&relay, real + 3_000).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("new"));
    assert_eq!(b.password("y.example", "u"), None);

    // 「차례 0 · 시계 1」 — 시계가 맞지 않아 바퀴를 멈추고 아무것도 적용하지 않는다.
    *relay.forged.borrow_mut() = Some((0, 1));
    assert_eq!(b.sync_at(&relay, real + 4_000, false), Err(SyncError::ClockSkew));
    // 시계는 맞추고 차례만 되돌려도, 아는 판보다 낡은 것은 적용하지 않는다.
    *relay.forged.borrow_mut() = Some((0, real + 5_000));
    b.sync_at(&relay, real + 5_000, false).unwrap();
    *relay.forged.borrow_mut() = None;
    assert_eq!(b.password("x.example", "u").as_deref(), Some("new"));
    assert_eq!(b.password("y.example", "u"), None, "지운 것도 되살아나지 않는다");
    let mut c = Device::new("c", &key);
    c.sync(&relay, real + 6_000).unwrap();
    assert_eq!(c.password("x.example", "u").as_deref(), Some("new"));
}

#[test]
fn a_retry_whose_local_value_could_not_be_read_is_kept_not_overridden() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    let mut b = Device::new("b", &key);
    a.edit(1_000, "x.example", "U", "v1");
    a.sync(&relay, 1_500).unwrap();
    b.sync(&relay, 1_600).unwrap();
    a.edit(2_000, "x.example", "U", "from-a");
    a.sync(&relay, 2_500).unwrap();
    // 쓰기와 바로 뒤 읽기가 함께 실패 — 못 썼을 때의 로컬 값을 모른다.
    b.store.fail_writes(true);
    b.store.fail_reads(true);
    b.sync(&relay, 2_600).unwrap();
    b.store.fail_writes(false);
    b.store.fail_reads(false);
    b.sync(&relay, 2_700).unwrap();
    a.sync(&relay, 2_800).unwrap();
    assert_eq!(b.password("x.example", "u").as_deref(), Some("from-a"), "모르는 채로 옛 로컬 값을 올리지 않고 다시 쓴다");
    assert_eq!(a.password("x.example", "u").as_deref(), Some("from-a"));
}

#[test]
fn a_wrong_key_never_touches_local_or_relay() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    a.edit(1_000, "x.example", "u", "p");
    a.sync(&relay, 2_000).unwrap();
    let before = relay.store.fetch(ACCOUNT, 0, 0).unwrap();

    let mut stranger = Device::new("s", &VaultKey::generate().unwrap());
    stranger.edit(1_000, "y.example", "u", "p");
    assert_eq!(stranger.sync(&relay, 3_000), Err(SyncError::KeyMismatch));
    assert_eq!(relay.store.fetch(ACCOUNT, 0, 0).unwrap().items, before.items);

    // 관문이 열쇠 표식은 맞추고 덩어리를 바꿔치기해도, 하나도 안 풀리면 아무것도 올리지 않는다.
    let id = before.items[0].id.clone();
    let forged = VaultKey::generate().unwrap().seal(ACCOUNT, &id, b"{}").unwrap();
    relay.replay(&key, &id, &forged);
    let mut fresh = Device::new("f", &key);
    fresh.edit(1_000, "z.example", "u", "p");
    assert_eq!(fresh.sync(&relay, 5_000), Err(SyncError::Undecryptable));
    assert_eq!(relay.store.fetch(ACCOUNT, 0, 0).unwrap().items.len(), 1);
}

#[test]
fn old_tombstones_are_swept_by_their_sealed_time() {
    let (relay, key) = vault();
    let mut a = Device::new("a", &key);
    a.edit(1_000, "x.example", "u", "p");
    a.sync(&relay, 2_000).unwrap();
    a.store.delete("x.example", "u").unwrap();
    a.sync(&relay, 3_000).unwrap();
    assert_eq!(relay.store.fetch(ACCOUNT, 0, 0).unwrap().items.len(), 1, "묘비는 남는다");
    a.sync(&relay, 3_000 + TOMBSTONE_KEEP_MS + 1).unwrap();
    assert!(relay.store.fetch(ACCOUNT, 0, 0).unwrap().items.is_empty(), "180일 지나면 걷는다");
    assert!(a.state.items.is_empty());
}

#[test]
fn no_vault_means_no_work() {
    let relay = Relay::new();
    let mut a = Device::new("a", &VaultKey::generate().unwrap());
    assert_eq!(a.sync(&relay, 1), Err(SyncError::NoVault));
}
