//! 동기화 한 바퀴: 관문의 새 항목을 받고, 로컬에서 바뀐 것을 올리고, 둘이 부딪히면 나중에 바뀐 쪽을 남긴다.
//! 관문의 차례(`rev`)는 「무엇이 새로 왔나」와 덮어쓰기 검사에만 쓰고, 이기는 쪽은 봉인 안의 (시각, 기기)로 가른다.
//! 받은 항목이 이 기기가 아는 판보다 낡았으면 적용하지 않고 아는 판을 다시 올린다 — 관문이 옛 덩어리를 새 차례로
//! 내밀어 비밀번호를 되돌리거나 지우지 못하게.

use std::collections::{BTreeMap, BTreeSet};

use zeroize::Zeroizing;

use super::crypto::VaultKey;
use super::record::{stamp, wins, Record, MAX_SKEW_MS, TOMBSTONE_KEEP_MS};
use super::server::{put_message, Fetched, Put, PutOutcome, RemoteItem};
use super::state::{Known, State};
use super::store::{storable, LoginStore, OWNER_EDIT};

#[derive(Debug, PartialEq, Eq)]
pub enum RemoteError {
    Unauthorized,
    Unsupported,
    KeyMismatch,
    /// 관문이 쓰기 서명을 받지 않았다 — 이 기기의 열쇠가 금고 열쇠가 아니다.
    BadSignature,
    Exists(String),
    NotFound,
    Failed,
}

/// 관문. 시험은 [`super::server::Store`] 를 그대로 쓰고, 앱은 HTTP 로 부른다.
pub trait Remote {
    fn init(&self, key_id: &str, sign_pub: &str) -> Result<(), RemoteError>;
    fn fetch(&self, since: u64) -> Result<Fetched, RemoteError>;
    fn put(&self, key_id: &str, puts: &[Put], sig: &str) -> Result<Vec<PutOutcome>, RemoteError>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SyncError {
    /// 관문에 아직 금고가 없다.
    NoVault,
    /// 이 기기의 열쇠가 관문 금고의 열쇠가 아니다 — 다시 등록해야 한다. 로컬은 덮지 않는다.
    KeyMismatch,
    /// 관문 항목이 하나도 안 풀린다 — 가짜 열쇠일 수 있어 아무것도 올리지 않는다.
    Undecryptable,
    /// 관문 시계가 이 기기 시계보다 너무 뒤진다(또는 이 기기 시계가 너무 앞선다). 어느 쪽이 맞는지 몰라 그 바퀴를 멈춘다 —
    /// 관문 시계로 제 시각을 깎으면 관문이 시계를 속여 옛 판을 이기게 할 수 있다.
    ClockSkew,
    Remote(RemoteError),
    Store,
}

impl From<RemoteError> for SyncError {
    fn from(error: RemoteError) -> Self {
        match error {
            RemoteError::KeyMismatch | RemoteError::BadSignature => Self::KeyMismatch,
            RemoteError::NotFound => Self::NoVault,
            other => Self::Remote(other),
        }
    }
}

/// 로그에 남기는 것은 이 개수뿐이다.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Counts {
    pub uploaded: usize,
    pub received: usize,
    pub tombstones: usize,
    /// 키체인이 지우기를 거절해 남겨 둔 묘비.
    pub held: usize,
    /// 한 바퀴에 너무 많이 사라져 사람 확인을 기다리는 로컬 지우기.
    pub deletions_held: usize,
    pub failed: usize,
    pub undecryptable: usize,
}

pub struct Engine<'a> {
    pub account: &'a str,
    pub device: &'a str,
    pub key: &'a VaultKey,
    pub store: &'a dyn LoginStore,
    pub remote: &'a dyn Remote,
    pub now_ms: u64,
    /// 사람이 「지우기 N개 — 확인」을 눌렀다. 아니면 한도를 넘는 로컬 지우기는 올리지 않는다.
    pub allow_bulk_delete: bool,
}

/// 한 바퀴에 올리는 로컬 지우기의 한도 — 5개 또는 아는 항목의 10% 중 큰 쪽. 목록을 잘못 읽어 비면 다 지운 것으로
/// 보이는데, 그것이 모든 기기로 번지지 않게 한다.
fn deletion_limit(live: usize) -> usize {
    5.max(live.div_ceil(10))
}

/// 아이디는 항목 이름처럼 소문자로 넣는다 — 관문 판과 로컬 항목의 철자가 대소문자만 달라도 같은 값으로 본다.
fn value_mac(key: &VaultKey, username: &str, password: &str) -> String {
    let text = Zeroizing::new(format!("{}\0{password}", username.to_lowercase()));
    key.value_mac(text.as_bytes())
}

/// 로컬에서 생긴 변경 하나와, 그것을 올릴 때 기대하는 관문 차례.
struct Outgoing {
    record: Record,
    expected_rev: u64,
    mac: String,
    local_ms: Option<u64>,
}

impl Engine<'_> {
    fn open(&self, item: &RemoteItem) -> Option<Record> {
        let plain = self.key.open(self.account, &item.id, &item.blob).ok()?;
        serde_json::from_slice::<Record>(&plain).ok().filter(|r| self.key.item_id(&r.host, &r.username) == item.id)
    }

    fn seal(&self, id: &str, record: &Record) -> anyhow::Result<String> {
        let plain = Zeroizing::new(serde_json::to_vec(record)?);
        self.key.seal(self.account, id, &plain)
    }

    /// 지금 로컬 값의 지문. 없으면 빈 값.
    fn local_mac(&self, host: &str, username: &str) -> Option<String> {
        match self.store.read(host, username) {
            Ok(Some(password)) => Some(value_mac(self.key, username, &password)),
            Ok(None) => Some(String::new()),
            Err(_) => None,
        }
    }

    /// 관문 항목을 로컬에 적용하고 그 결과의 [`Known`] 을 준다.
    fn apply(&self, item: &RemoteItem, record: &Record, counts: &mut Counts) -> Known {
        let mut known = Known {
            rev: item.rev,
            changed_at_ms: record.changed_at_ms,
            device: record.device.clone(),
            deleted: record.deleted,
            ..Default::default()
        };
        let retry = |known: &mut Known, counts: &mut Counts| {
            known.retry_blob = Some(item.blob.clone());
            known.retry_local = self.local_mac(&record.host, &record.username);
            counts.failed += 1;
        };
        if record.deleted {
            match self.store.read(&record.host, &record.username) {
                Ok(None) => counts.tombstones += 1,
                Ok(Some(password)) => match self.store.delete(&record.host, &record.username) {
                    Ok(()) => counts.tombstones += 1,
                    Err(error) if error.to_string() == OWNER_EDIT => {
                        known.held_mac = Some(value_mac(self.key, &record.username, &password));
                        counts.held += 1;
                    }
                    Err(_) => retry(&mut known, counts),
                },
                Err(_) => retry(&mut known, counts),
            }
            return known;
        }
        known.mac = value_mac(self.key, &record.username, &record.password);
        if !storable(&record.host, &record.username) {
            counts.failed += 1;
            return known;
        }
        match self.store.write(&record.host, &record.username, &record.password) {
            Ok(()) => counts.received += 1,
            Err(_) => retry(&mut known, counts),
        }
        known
    }

    /// 로컬을 훑어 바뀐 것을 모은다. 값은 수정 시각이 달라진 항목만 읽는다.
    fn local_changes(&self, state: &mut State, relay_now: u64, counts: &mut Counts) -> Result<BTreeMap<String, Outgoing>, SyncError> {
        let entries = self.store.list().map_err(|_| SyncError::Store)?;
        let mut seen = BTreeSet::new();
        let mut out = BTreeMap::new();
        for entry in entries {
            if !storable(&entry.host, &entry.username) {
                continue;
            }
            let id = self.key.item_id(&entry.host, &entry.username);
            if !seen.insert(id.clone()) {
                continue;
            }
            let (floor, expected_rev, retrying) = match state.items.get(&id) {
                Some(k) if k.retry_blob.is_none() && k.local_ms.is_some() && k.local_ms == entry.modified_ms && k.held_mac.is_none() => continue,
                Some(k) => (k.changed_at_ms + 1, k.rev, k.retry_blob.is_some()),
                None => (0, 0, false),
            };
            let Ok(Some(password)) = self.store.read(&entry.host, &entry.username) else { continue };
            let mac = value_mac(self.key, &entry.username, &password);
            if let Some(k) = state.items.get_mut(&id) {
                if retrying {
                    // 못 쓴 관문 항목을 다시 쓰기 전에 — 그사이 사람이 고쳤으면 재시도를 버리고 고친 것을 올린다.
                    // 못 썼을 때의 로컬 값을 못 읽었으면(None) 고쳤는지 알 수 없어 재시도를 그대로 둔다.
                    match k.retry_local.as_deref() {
                        None => continue,
                        Some(before) if before == mac => continue,
                        Some(_) => {}
                    }
                    k.retry_blob = None;
                    k.retry_local = None;
                } else {
                    let unchanged = if k.deleted { k.held_mac.as_deref() == Some(mac.as_str()) } else { k.mac == mac };
                    if unchanged {
                        k.local_ms = entry.modified_ms;
                        continue;
                    }
                }
            }
            let local = entry.modified_ms.filter(|t| *t <= self.now_ms).unwrap_or(self.now_ms);
            let record = Record {
                host: entry.host.clone(),
                username: entry.username.clone(),
                password,
                changed_at_ms: stamp(local, relay_now, floor),
                deleted: false,
                device: self.device.into(),
            };
            out.insert(id, Outgoing { record, expected_rev, mac, local_ms: entry.modified_ms });
        }
        let mut gone = Vec::new();
        for (id, k) in state.items.iter_mut() {
            if k.deleted || seen.contains(id) {
                continue;
            }
            if k.retry_blob.is_some() {
                // 못 썼을 때 로컬에 값이 있었는데 지금 없으면 사람이 지운 것이다.
                if k.retry_local.as_deref().is_some_and(|m| !m.is_empty()) {
                    k.retry_blob = None;
                    k.retry_local = None;
                } else {
                    continue;
                }
            }
            gone.push(id.clone());
        }
        let live = state.items.values().filter(|k| !k.deleted).count();
        if gone.len() > deletion_limit(live) && !self.allow_bulk_delete {
            counts.deletions_held = gone.len();
            return Ok(out);
        }
        for id in gone {
            let k = &state.items[&id];
            // 묘비에는 사이트·아이디가 필요해 관문 항목을 풀어 쓴다 — 로컬 상태엔 평문이 없다.
            out.insert(id.clone(), Outgoing {
                record: Record::tombstone("", "", stamp(self.now_ms, relay_now, k.changed_at_ms + 1), self.device),
                expected_rev: k.rev,
                mac: String::new(),
                local_ms: None,
            });
        }
        Ok(out)
    }

    /// 받은 항목이 아는 판보다 낡았을 때 — 아는 판을 그 시각·기기 그대로 다시 올린다.
    fn restore(&self, item: &RemoteItem, theirs: &Record, known: &Known) -> Option<Outgoing> {
        let record = if known.deleted {
            Record::tombstone(&theirs.host, &theirs.username, known.changed_at_ms, &known.device)
        } else {
            let password = self.store.read(&theirs.host, &theirs.username).ok()??;
            if value_mac(self.key, &theirs.username, &password) != known.mac {
                return None;
            }
            Record {
                host: theirs.host.clone(),
                username: theirs.username.clone(),
                password,
                changed_at_ms: known.changed_at_ms,
                deleted: false,
                device: known.device.clone(),
            }
        };
        Some(Outgoing { record, expected_rev: item.rev, mac: known.mac.clone(), local_ms: known.local_ms })
    }

    /// 한 바퀴. 상태는 바뀐 만큼 고쳐 두고, 저장은 부르는 쪽이 한다.
    pub fn round(&self, state: &mut State) -> Result<Counts, SyncError> {
        let key_id = self.key.key_id();
        let mut fetched = self.remote.fetch(state.revision)?;
        if fetched.key_id.is_empty() {
            return Err(SyncError::NoVault);
        }
        if fetched.key_id != key_id {
            return Err(SyncError::KeyMismatch);
        }
        // 다른 기기에서 옮겨 온 상태나 다른 금고면 처음부터 다시 맞춘다.
        if state.device != self.device || state.key_id != key_id {
            *state = State { device: self.device.into(), key_id: key_id.clone(), ..State::default() };
            fetched = self.remote.fetch(0)?;
        } else if state.revision > fetched.revision {
            // 관문 차례가 되돌아갔다 — 차례만 처음부터 다시 받고, 아는 판(시각·기기·값 지문)은 바닥으로 남긴다.
            // 판까지 버리면 관문이 차례를 되돌려 옛 덩어리를 다시 이기게 할 수 있다.
            state.revision = 0;
            for known in state.items.values_mut() {
                known.rev = 0;
            }
            fetched = self.remote.fetch(0)?;
        }
        let relay_now = fetched.now_ms;
        if relay_now == 0 || relay_now.saturating_add(MAX_SKEW_MS) < self.now_ms {
            return Err(SyncError::ClockSkew);
        }
        let mut counts = Counts::default();
        let opened: Vec<(RemoteItem, Option<Record>)> = fetched.items.into_iter().map(|item| {
            let record = self.open(&item);
            (item, record)
        }).collect();
        if !opened.is_empty() && opened.iter().all(|(_, r)| r.is_none()) {
            return Err(SyncError::Undecryptable);
        }
        let mut outgoing = self.local_changes(state, relay_now, &mut counts)?;
        // 이번 바퀴에 실패한 것은 다음 바퀴에 — 같은 바퀴에 곧바로 다시 쓰지 않는다.
        let carried: Vec<String> = state.items.iter().filter(|(_, k)| k.retry_blob.is_some()).map(|(id, _)| id.clone()).collect();
        let mut names: BTreeMap<String, (String, String)> = opened
            .iter()
            .filter_map(|(item, r)| r.as_ref().map(|r| (item.id.clone(), (r.host.clone(), r.username.clone()))))
            .collect();

        for (item, record) in &opened {
            let Some(record) = record else {
                counts.undecryptable += 1;
                continue;
            };
            let known = state.items.get(&item.id).cloned();
            if known.as_ref().is_some_and(|k| k.rev == item.rev && k.retry_blob.is_none()) {
                continue;
            }
            if let Some(mine) = outgoing.get_mut(&item.id) {
                if wins(mine.record.version(), record.version()) {
                    mine.expected_rev = item.rev;
                    continue;
                }
                outgoing.remove(&item.id);
            } else if let Some(k) = known.as_ref().filter(|k| !wins(record.version(), k.version())) {
                if record.version() == k.version() {
                    // 같은 판이 새 차례로 다시 놓였다 — 내용은 봉인이 같다고 말한다.
                    if let Some(entry) = state.items.get_mut(&item.id) {
                        entry.rev = item.rev;
                    }
                } else if let Some(restore) = self.restore(item, record, k) {
                    outgoing.insert(item.id.clone(), restore);
                }
                continue;
            }
            let known = self.apply(item, record, &mut counts);
            state.items.insert(item.id.clone(), known);
        }
        state.revision = fetched.revision;

        self.retry_held(state, &carried, &mut counts);
        self.upload(state, &mut outgoing, &mut names, &mut counts)?;
        self.sweep_tombstones(state, &key_id);
        counts.held = state.items.values().filter(|k| k.held_mac.is_some()).count();
        Ok(counts)
    }

    /// 지난 바퀴에 로컬에 못 쓴 관문 항목을 다시 쓴다. 그사이 사람이 고친 것은 `local_changes` 가 먼저 걸렀다.
    fn retry_held(&self, state: &mut State, carried: &[String], counts: &mut Counts) {
        let retry: Vec<(String, Known)> =
            carried.iter().filter_map(|id| state.items.get(id).filter(|k| k.retry_blob.is_some()).map(|k| (id.clone(), k.clone()))).collect();
        for (id, k) in retry {
            let item = RemoteItem { id: id.clone(), rev: k.rev, at_ms: 0, blob: k.retry_blob.clone().unwrap_or_default() };
            let Some(record) = self.open(&item) else { continue };
            let known = self.apply(&item, &record, counts);
            state.items.insert(id, known);
        }
    }

    fn upload(
        &self,
        state: &mut State,
        outgoing: &mut BTreeMap<String, Outgoing>,
        names: &mut BTreeMap<String, (String, String)>,
        counts: &mut Counts,
    ) -> Result<(), SyncError> {
        let key_id = self.key.key_id();
        let unnamed = outgoing.iter().any(|(id, out)| out.record.deleted && out.record.host.is_empty() && !names.contains_key(id));
        if unnamed {
            self.fill_names(names);
        }
        for _ in 0..4 {
            if outgoing.is_empty() {
                return Ok(());
            }
            let mut puts = Vec::new();
            for (id, out) in outgoing.iter_mut() {
                if out.record.deleted && out.record.host.is_empty() {
                    let Some((host, username)) = names.get(id).cloned() else { continue };
                    out.record.host = host;
                    out.record.username = username;
                }
                match self.seal(id, &out.record) {
                    Ok(blob) => puts.push(Put { id: id.clone(), expected_rev: out.expected_rev, blob: Some(blob) }),
                    Err(_) => counts.failed += 1,
                }
            }
            outgoing.retain(|id, _| puts.iter().any(|p| &p.id == id));
            if puts.is_empty() {
                return Ok(());
            }
            let mut next = BTreeMap::new();
            for chunk in puts.chunks(super::server::MAX_PUT) {
                let sig = self.key.sign(&put_message(self.account, &key_id, chunk));
                for outcome in self.remote.put(&key_id, chunk, &sig)? {
                    match outcome {
                        PutOutcome::Ok { id, rev, .. } => {
                            let Some(out) = outgoing.remove(&id) else { continue };
                            counts.uploaded += 1;
                            state.items.insert(id, Known {
                                rev,
                                changed_at_ms: out.record.changed_at_ms,
                                device: out.record.device.clone(),
                                deleted: out.record.deleted,
                                mac: out.mac,
                                local_ms: out.local_ms,
                                ..Default::default()
                            });
                        }
                        PutOutcome::Conflict { id, current } => {
                            let Some(mut out) = outgoing.remove(&id) else { continue };
                            match current {
                                None => {
                                    out.expected_rev = 0;
                                    next.insert(id, out);
                                }
                                Some(item) => {
                                    let Some(record) = self.open(&item) else {
                                        counts.undecryptable += 1;
                                        continue;
                                    };
                                    names.insert(id.clone(), (record.host.clone(), record.username.clone()));
                                    if wins(out.record.version(), record.version()) {
                                        out.expected_rev = item.rev;
                                        next.insert(id, out);
                                    } else {
                                        let known = self.apply(&item, &record, counts);
                                        state.items.insert(id, known);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            *outgoing = next;
        }
        counts.failed += outgoing.len();
        Ok(())
    }

    /// 로컬 상태에 평문이 없어, 지운 항목의 이름은 관문 사본을 풀어 얻는다. 한 바퀴에 한 번만 받는다.
    fn fill_names(&self, names: &mut BTreeMap<String, (String, String)>) {
        let Ok(fetched) = self.remote.fetch(0) else { return };
        for item in fetched.items {
            if let Some(record) = self.open(&item) {
                names.entry(item.id.clone()).or_insert((record.host.clone(), record.username.clone()));
            }
        }
    }

    /// 180일 지난 묘비를 관문에서 걷는다. 시각은 봉인 안의 것이라 관문이 앞당기지 못한다.
    fn sweep_tombstones(&self, state: &mut State, key_id: &str) {
        let old: Vec<Put> = state
            .items
            .iter()
            .filter(|(_, k)| k.deleted && k.rev > 0 && k.held_mac.is_none() && k.changed_at_ms.saturating_add(TOMBSTONE_KEEP_MS) < self.now_ms)
            .map(|(id, k)| Put { id: id.clone(), expected_rev: k.rev, blob: None })
            .take(super::server::MAX_PUT)
            .collect();
        if old.is_empty() {
            return;
        }
        let sig = self.key.sign(&put_message(self.account, key_id, &old));
        if let Ok(outcomes) = self.remote.put(key_id, &old, &sig) {
            for outcome in outcomes {
                match outcome {
                    // 다른 기기가 이미 걷었으면(관문에 없음) 이쪽도 잊는다 — 매 바퀴 같은 걷기를 되풀이하지 않게.
                    PutOutcome::Ok { id, .. } | PutOutcome::Conflict { id, current: None } => {
                        state.items.remove(&id);
                    }
                    PutOutcome::Conflict { .. } => {}
                }
            }
        }
    }
}
