//! 동기화 한 바퀴: 관문의 새 항목을 받고, 브라우저에서 바뀐 것을 찾아, 묶음마다 나중 것을 남긴 기록으로 브라우저와 관문을
//! 함께 맞춘다. 판정은 봉인 안의 (시각, 기기)로만 하고, 관문 시계 규칙·옛 덩어리 무시는 로그인 금고와 같다.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha2::Digest as _;

use super::browser::{Browser, BrowserError, Collection, Device, Mark, Op, Read};
use super::order::{self, Sibling};
use super::record::{MarkRecord, Stamp, TabsRecord};
use super::state::{KnownMark, State};
use super::tree;
use crate::login_vault::crypto::VaultKey;
use crate::login_vault::engine::{Remote, RemoteError};
use crate::login_vault::record::{stamp, MAX_SKEW_MS, TOMBSTONE_KEEP_MS};
use crate::login_vault::server::{put_message, Limits, Put, PutOutcome, RemoteItem, MAX_PUT};

/// 관문 보관소의 한도. 탭 목록 덩어리(봉인 64KB 의 base64)가 들어가고, 북마크 2만 개까지.
pub const RELAY_LIMITS: Limits = Limits { items: 20_000, blob: 96 * 1024, file: 64 << 20 };
/// 관문 본문 한도(1MB) 안에 들도록 한 번에 올리는 덩어리 크기.
const PUT_BYTES: usize = 768 * 1024;

/// 북마크 덩어리는 로그인처럼 256바이트 단위, 탭 목록은 4KB 단위로 채운다.
const MARK_PAD: usize = 256;
pub const MARK_MAX: usize = 6 * 1024;
const TABS_PAD: usize = 4096;
pub const TABS_MAX: usize = 64 * 1024;
/// 탭은 바뀔 때마다가 아니라 이만큼 묶어서 올린다 — 관문이 활동을 덜 엿보게.
const TABS_EVERY_MS: u64 = 30_000;
/// 한 바퀴에 이만큼 넘게, 그리고 아는 북마크의 절반 넘게 사라지면 올리지 않고 사람 확인을 기다린다.
const BULK_DELETE: usize = 20;

#[derive(Debug, PartialEq, Eq)]
pub enum SyncError {
    KeyMismatch,
    Undecryptable,
    ClockSkew,
    Remote(RemoteError),
    Browser(String),
}

impl From<RemoteError> for SyncError {
    fn from(error: RemoteError) -> Self {
        match error {
            RemoteError::KeyMismatch | RemoteError::BadSignature => Self::KeyMismatch,
            other => Self::Remote(other),
        }
    }
}

/// 로그에 남기는 것은 이 개수뿐이다.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Counts {
    pub uploaded: usize,
    /// 브라우저에 한 고치기 수.
    pub applied: usize,
    pub deletions_held: usize,
    /// 사람이 그사이 고쳐 다음 바퀴로 미룬 모음.
    pub stale: usize,
    pub failed: usize,
    pub undecryptable: usize,
    pub tabs_sent: bool,
    pub devices_shown: usize,
    /// 브라우저가 꺼져 있어 북마크·탭을 건너뛰었다.
    pub browser_away: bool,
}

pub struct Engine<'a> {
    pub account: &'a str,
    pub device: &'a str,
    pub device_name: &'a str,
    /// 금고 열쇠의 `browser-sync` 갈래([`VaultKey::subkey`]).
    pub key: &'a VaultKey,
    pub browser: &'a dyn Browser,
    pub remote: &'a dyn Remote,
    pub now_ms: u64,
    pub allow_bulk_delete: bool,
    pub send_tabs: bool,
}

fn mark_name(collection: &str, sync_id: &str) -> String {
    format!("mark\n{collection}\n{sync_id}")
}

fn tabs_name(device: &str) -> String {
    format!("tabs\n{device}")
}

/// 브라우저가 더할 때 받는 id 꼴 — 남이 보낸 기록의 id 도 이것만 받는다.
fn valid_sync_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_pos(pos: &str) -> bool {
    pos.len() <= 64 && pos.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn new_sync_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 받은 항목 하나를 푼 것.
enum Opened {
    Mark(MarkRecord),
    Tabs(TabsRecord),
}

/// 브라우저 트리의 한 자리.
struct Local<'m> {
    mark: &'m Mark,
    parent: String,
    path: Vec<String>,
}

fn flatten<'m>(marks: &'m [Mark], parent: &str, path: &[String], out: &mut Vec<Local<'m>>) {
    for m in marks {
        out.push(Local { mark: m, parent: parent.into(), path: path.to_vec() });
        if m.folder() {
            let mut inner = path.to_vec();
            inner.push(m.title.clone());
            flatten(&m.children, &m.id, &inner, out);
        }
    }
}

/// 이번 바퀴에 관문에 올릴 것 하나.
struct Outgoing {
    id: String,
    expected_rev: u64,
    blob: String,
    kind: Pending,
}

enum Pending {
    Mark,
    Tabs { rev: String },
}

/// 브라우저 트리를 흉내 내 최소한의 고치기를 만든다. `Tree.Move` 와 같이 옮길 자리는 옮기는 것을 뺀 순서다.
struct Sim {
    parent: HashMap<String, String>,
    children: HashMap<String, Vec<String>>,
}

impl Sim {
    fn new(locals: &[Local]) -> Self {
        let mut sim = Sim { parent: HashMap::new(), children: HashMap::new() };
        for l in locals {
            sim.parent.insert(l.mark.id.clone(), l.parent.clone());
            sim.children.entry(l.parent.clone()).or_default().push(l.mark.id.clone());
        }
        sim
    }

    fn at(&self, id: &str) -> Option<(&str, usize)> {
        let parent = self.parent.get(id)?;
        let index = self.children.get(parent)?.iter().position(|c| c == id)?;
        Some((parent, index))
    }

    fn place(&mut self, id: &str, parent: &str, index: usize) {
        if let Some(old) = self.parent.get(id).cloned() {
            if let Some(list) = self.children.get_mut(&old) {
                list.retain(|c| c != id);
            }
        }
        let list = self.children.entry(parent.into()).or_default();
        list.insert(index.min(list.len()), id.into());
        self.parent.insert(id.into(), parent.into());
    }
}

impl Engine<'_> {
    fn mac(&self, title: &str, url: &str) -> String {
        self.key.value_mac(format!("{title}\0{url}").as_bytes())
    }

    fn open(&self, item: &RemoteItem) -> Option<Opened> {
        let plain = self.key.open(self.account, &item.id, &item.blob).ok()?;
        if let Ok(mut r) = serde_json::from_slice::<MarkRecord>(&plain) {
            if valid_sync_id(&r.sync_id) && self.key.name_id(&mark_name(&r.collection, &r.sync_id)) == item.id {
                if !valid_pos(&r.pos) {
                    r.pos.clear();
                }
                if r.folder && r.title.trim().is_empty() {
                    r.title = "이름 없는 폴더".into();
                }
                return Some(Opened::Mark(r));
            }
        }
        let tabs = serde_json::from_slice::<TabsRecord>(&plain).ok()?;
        (self.key.name_id(&tabs_name(&tabs.device)) == item.id).then_some(Opened::Tabs(tabs))
    }

    fn seal(&self, id: &str, plain: &[u8], pad: usize, max: usize) -> anyhow::Result<String> {
        self.key.seal_padded(self.account, id, plain, pad, max)
    }

    pub fn round(&self, state: &mut State) -> Result<Counts, SyncError> {
        let key_id = self.key.key_id();
        let mut fetched = self.remote.fetch(state.revision)?;
        if fetched.key_id.is_empty() {
            // 금고 열쇠를 가진 기기는 이미 사람이 믿은 기기다 — 갈래의 관문 기록은 처음 온 기기가 만든다.
            match self.remote.init(&key_id, &self.key.sign_pub()) {
                Ok(()) | Err(RemoteError::Exists(_)) => {}
                Err(error) => return Err(error.into()),
            }
            fetched = self.remote.fetch(0)?;
        }
        if fetched.key_id != key_id {
            return Err(SyncError::KeyMismatch);
        }
        if state.device != self.device || state.key_id != key_id {
            *state = State { device: self.device.into(), key_id: key_id.clone(), ..State::default() };
            fetched = self.remote.fetch(0)?;
        } else if state.revision > fetched.revision {
            state.revision = 0;
            for known in state.marks.values_mut() {
                known.rev = 0;
            }
            fetched = self.remote.fetch(0)?;
        }
        let relay_now = fetched.now_ms;
        if relay_now == 0 || relay_now.saturating_add(MAX_SKEW_MS) < self.now_ms {
            return Err(SyncError::ClockSkew);
        }
        let read = match self.browser.read() {
            Ok(read) => read,
            Err(BrowserError::NotRunning) => return Ok(Counts { browser_away: true, ..Counts::default() }),
            Err(error) => return Err(SyncError::Browser(format!("{error:?}"))),
        };
        if state.revision > 0 && read.collections.iter().any(|c| !state.collections.contains(&c.key)) {
            fetched = self.remote.fetch(0)?;
        }
        let mut counts = Counts::default();
        let mut marks: BTreeMap<String, BTreeMap<String, (RemoteItem, MarkRecord)>> = BTreeMap::new();
        let mut tabs: Vec<(RemoteItem, TabsRecord)> = Vec::new();
        let total = fetched.items.len();
        for item in fetched.items {
            match self.open(&item) {
                Some(Opened::Mark(r)) => {
                    marks.entry(r.collection.clone()).or_default().insert(r.sync_id.clone(), (item, r));
                }
                Some(Opened::Tabs(t)) => tabs.push((item, t)),
                None => counts.undecryptable += 1,
            }
        }
        if total > 0 && counts.undecryptable == total {
            return Err(SyncError::Undecryptable);
        }

        let mut outgoing = Vec::new();
        let empty = BTreeMap::new();
        let mut complete = true;
        for c in &read.collections {
            let remote = marks.get(&c.key).unwrap_or(&empty);
            match self.collection(state, c, remote, relay_now, &mut counts, &mut outgoing) {
                Ok(()) => {
                    state.collections.insert(c.key.clone());
                }
                Err(BrowserError::Stale) => {
                    counts.stale += 1;
                    complete = false;
                }
                Err(BrowserError::NotRunning) => {
                    counts.browser_away = true;
                    complete = false;
                }
                Err(BrowserError::Failed(_)) => {
                    counts.failed += 1;
                    complete = false;
                }
            }
        }
        self.tabs(state, &read, tabs, relay_now, &mut counts, &mut outgoing);
        // 미룬 모음의 관문 항목은 다음 바퀴에 다시 받아야 하니 차례를 앞당기지 않는다.
        if complete {
            state.revision = fetched.revision;
        }
        self.upload(state, outgoing, &key_id, &mut counts)?;
        self.sweep(state, &key_id);
        Ok(counts)
    }

    /// 모음 하나를 맞춘다. 브라우저에 고치기를 한 뒤에만 상태를 고친다 — `Stale` 이면 아무것도 안 바뀐다.
    fn collection(
        &self,
        state: &mut State,
        c: &Collection,
        remote: &BTreeMap<String, (RemoteItem, MarkRecord)>,
        relay_now: u64,
        counts: &mut Counts,
        outgoing: &mut Vec<Outgoing>,
    ) -> Result<(), BrowserError> {
        let mut locals = Vec::new();
        flatten(&c.marks, "", &[], &mut locals);
        let known: BTreeMap<String, KnownMark> =
            state.marks.values().filter(|k| k.collection == c.key).map(|k| (k.sync_id.clone(), k.clone())).collect();
        let mut sync_of: HashMap<String, String> = known.values().map(|k| (k.local_id.clone(), k.sync_id.clone())).collect();
        let mut adopted: BTreeSet<String> = BTreeSet::new();
        if !state.collections.contains(&c.key) {
            self.match_first(&locals, remote, &mut sync_of, &mut adopted);
        }
        for l in &locals {
            // 브라우저 id 는 무작위라 그대로 동기화 id 로 쓴다 — 받는 기기도 같은 id 로 더한다. 겹치면 새로 뽑는다.
            let free = valid_sync_id(&l.mark.id) && !known.contains_key(&l.mark.id) && !remote.contains_key(&l.mark.id);
            sync_of.entry(l.mark.id.clone()).or_insert_with(|| if free { l.mark.id.clone() } else { new_sync_id() });
        }
        let local_of_sync: HashMap<&str, &Local> = locals.iter().map(|l| (sync_of[&l.mark.id].as_str(), l)).collect();

        let stamp_now = |floor: u64| Stamp::new(stamp(self.now_ms, relay_now, floor), self.device);
        let mut records: BTreeMap<String, MarkRecord> = BTreeMap::new();
        // 로컬에 있는 것: 내용 바뀜을 찾는다. 자리는 형제 순서를 다 본 뒤에.
        for l in &locals {
            let sid = sync_of[&l.mark.id].clone();
            if adopted.contains(&sid) {
                continue;
            }
            let record = match known.get(&sid) {
                Some(k) => {
                    let mut r = k.record(l.mark.title.clone(), l.mark.url.clone());
                    if self.mac(&l.mark.title, &l.mark.url) != k.content_mac {
                        r.content = stamp_now(k.content.at + 1);
                    }
                    if r.deleted.is_some() && !r.alive() {
                        // 묘비를 받은 뒤에도 로컬에 있다 — 지우기를 못 했거나 사람이 다시 만든 것이다. 되살린다.
                        r.content = stamp_now(r.latest() + 1);
                    }
                    r
                }
                None => MarkRecord {
                    collection: c.key.clone(),
                    sync_id: sid.clone(),
                    folder: l.mark.folder(),
                    title: l.mark.title.clone(),
                    url: l.mark.url.clone(),
                    added: l.mark.added,
                    content: stamp_now(0),
                    place: Stamp::default(),
                    ..Default::default()
                },
            };
            records.insert(sid, record);
        }
        // 자리: 부모마다 로컬 순서대로 아는 자리를 남기고 나머지에 새 자리를 준다.
        let mut groups: BTreeMap<&str, Vec<&Local>> = BTreeMap::new();
        for l in &locals {
            groups.entry(l.parent.as_str()).or_default().push(l);
        }
        for (parent_local, siblings) in &groups {
            let parent_sync = if parent_local.is_empty() { String::new() } else { sync_of[*parent_local].clone() };
            let ids: Vec<String> = siblings.iter().map(|l| sync_of[&l.mark.id].clone()).collect();
            let order: Vec<Sibling> = ids
                .iter()
                .map(|sid| {
                    let at = match known.get(sid) {
                        Some(k) => Some((&k.parent, &k.pos)),
                        None if adopted.contains(sid) => remote.get(sid).map(|(_, r)| (&r.parent, &r.pos)),
                        None => None,
                    };
                    Sibling { id: sid, pos: at.filter(|(p, pos)| **p == parent_sync && !pos.is_empty()).map(|(_, pos)| pos.as_str()) }
                })
                .collect();
            for (sid, (pos, moved)) in ids.iter().zip(order::place(&order)) {
                if adopted.contains(sid) || !moved {
                    continue;
                }
                if let Some(r) = records.get_mut(sid) {
                    let floor = known.get(sid).map_or(0, |k| k.place.at + 1);
                    r.parent = parent_sync.clone();
                    r.pos = pos;
                    r.place = stamp_now(floor);
                }
            }
        }
        // 로컬에 없는 아는 것: 사람이 지운 것이다. 너무 많이 사라졌으면 이번엔 손대지 않는다.
        let gone: Vec<&KnownMark> = known.values().filter(|k| k.alive() && !local_of_sync.contains_key(k.sync_id.as_str())).collect();
        let live = known.values().filter(|k| k.alive()).count();
        let mut held: BTreeSet<String> = BTreeSet::new();
        if gone.len() > BULK_DELETE && gone.len() * 2 > live && !self.allow_bulk_delete {
            counts.deletions_held += gone.len();
            held.extend(gone.iter().map(|k| k.sync_id.clone()));
        } else {
            for k in &gone {
                let mut r = k.record(String::new(), String::new());
                r.deleted = Some(stamp_now(r.latest() + 1));
                records.insert(k.sync_id.clone(), r);
            }
        }
        // 지운 채 아는 것(묘비)과 관문에서 온 것을 합친다.
        for k in known.values() {
            if !records.contains_key(&k.sync_id) && !held.contains(&k.sync_id) {
                records.insert(k.sync_id.clone(), k.record(String::new(), String::new()));
            }
        }
        for (sid, (_, theirs)) in remote {
            if held.contains(sid) {
                continue;
            }
            let merged = match records.get(sid) {
                Some(mine) => mine.merge(theirs),
                None => theirs.clone(),
            };
            records.insert(sid.clone(), merged);
        }
        let ops = self.ops(&locals, &records, &sync_of);
        if !ops.is_empty() {
            self.browser.apply(&c.key, &c.rev, &ops)?;
            counts.applied += ops.len();
        }

        for (sid, r) in &records {
            let item_id = self.key.name_id(&mark_name(&c.key, sid));
            let previous = state.marks.get(&item_id);
            let remote_item = remote.get(sid).map(|(item, theirs)| (item.rev, theirs));
            let rev = remote_item.as_ref().map_or_else(|| previous.map_or(0, |k| k.rev), |(rev, _)| *rev);
            // 관문 판과 다르거나, 관문에서 온 것 없이 아는 판이 바뀌었으면 올린다.
            let upload = match &remote_item {
                Some((_, theirs)) => *theirs != r,
                None => !previous.is_some_and(|k| k.content == r.content && k.place == r.place && k.deleted == r.deleted),
            };
            if upload {
                match serde_json::to_vec(r).map_err(anyhow::Error::from).and_then(|plain| self.seal(&item_id, &plain, MARK_PAD, MARK_MAX)) {
                    Ok(blob) => outgoing.push(Outgoing { id: item_id.clone(), expected_rev: rev, blob, kind: Pending::Mark }),
                    Err(_) => counts.failed += 1,
                }
            }
            let local_id = local_of_sync.get(sid.as_str()).map_or_else(|| sid.clone(), |l| l.mark.id.clone());
            state.marks.insert(item_id, KnownMark {
                collection: c.key.clone(),
                sync_id: sid.clone(),
                local_id,
                rev,
                folder: r.folder,
                content: r.content.clone(),
                content_mac: if r.alive() { self.mac(&r.title, &r.url) } else { String::new() },
                added: r.added,
                parent: r.parent.clone(),
                pos: r.pos.clone(),
                place: r.place.clone(),
                deleted: r.deleted.clone(),
            });
        }
        Ok(())
    }

    /// 처음 붙는 모음: (폴더 경로, 제목, 주소)가 같은 로컬 항목을 관문 항목에 짝짓는다 — 같은 북마크를 가져온 두 기기가
    /// 모든 것을 두 벌로 갖지 않게. 짝지은 것은 관문 판을 그대로 받는다.
    fn match_first(
        &self,
        locals: &[Local],
        remote: &BTreeMap<String, (RemoteItem, MarkRecord)>,
        sync_of: &mut HashMap<String, String>,
        adopted: &mut BTreeSet<String>,
    ) {
        let records: BTreeMap<String, MarkRecord> = remote.iter().map(|(sid, (_, r))| (sid.clone(), r.clone())).collect();
        let layout = tree::layout(&records);
        let mut free: HashMap<(Vec<String>, bool, String, String), Vec<String>> = HashMap::new();
        let mut walk = vec![(String::new(), Vec::<String>::new())];
        while let Some((parent, path)) = walk.pop() {
            for sid in layout.get(&parent).into_iter().flatten() {
                let r = &records[sid];
                free.entry((path.clone(), r.folder, r.title.clone(), r.url.clone())).or_default().push(sid.clone());
                if r.folder {
                    let mut inner = path.clone();
                    inner.push(r.title.clone());
                    walk.push((sid.clone(), inner));
                }
            }
        }
        for l in locals {
            let key = (l.path.clone(), l.mark.folder(), l.mark.title.clone(), l.mark.url.clone());
            if let Some(sid) = free.get_mut(&key).and_then(|ids| (!ids.is_empty()).then(|| ids.remove(0))) {
                sync_of.insert(l.mark.id.clone(), sid.clone());
                adopted.insert(sid);
            }
        }
    }

    /// 계산한 트리가 되도록 브라우저 고치기를 만든다: 위에서부터 더하기·옮기기·고치기, 그다음 남은 것 지우기.
    fn ops(&self, locals: &[Local], records: &BTreeMap<String, MarkRecord>, sync_of: &HashMap<String, String>) -> Vec<Op> {
        let layout = tree::layout(records);
        let local_of: HashMap<&str, &Local> = locals.iter().map(|l| (sync_of[&l.mark.id].as_str(), l)).collect();
        let lid = |sid: &str| local_of.get(sid).map_or_else(|| sid.to_string(), |l| l.mark.id.clone());
        let mut sim = Sim::new(locals);
        let mut ops = Vec::new();
        let mut wanted: BTreeSet<String> = BTreeSet::new();
        let mut walk = vec![String::new()];
        while let Some(parent_sid) = walk.pop() {
            let parent = if parent_sid.is_empty() { String::new() } else { lid(&parent_sid) };
            for (i, sid) in layout.get(&parent_sid).into_iter().flatten().enumerate() {
                let r = &records[sid];
                let id = lid(sid);
                wanted.insert(id.clone());
                match (sim.at(&id), local_of.get(sid.as_str())) {
                    (None, _) => {
                        ops.push(Op {
                            op: "add".into(),
                            id: id.clone(),
                            parent: parent.clone(),
                            index: i as i64,
                            folder: r.folder,
                            title: Some(r.title.clone()),
                            url: (!r.folder).then(|| r.url.clone()),
                            added: r.added,
                        });
                        sim.place(&id, &parent, i);
                    }
                    (Some(at), local) => {
                        if at != (parent.as_str(), i) {
                            ops.push(Op { op: "move".into(), id: id.clone(), parent: parent.clone(), index: i as i64, ..Op::default() });
                            sim.place(&id, &parent, i);
                        }
                        if let Some(l) = local.filter(|l| l.mark.title != r.title || l.mark.url != r.url) {
                            ops.push(Op {
                                op: "edit".into(),
                                id: id.clone(),
                                title: Some(r.title.clone()),
                                url: (!r.folder && l.mark.url != r.url).then(|| r.url.clone()),
                                ..Op::default()
                            });
                        }
                    }
                }
                if r.folder {
                    walk.push(sid.clone());
                }
            }
        }
        // 맨 위의 것만 지운다 — 폴더를 지우면 안의 것도 같이 간다(살아 있는 것은 위에서 이미 옮겼다).
        for l in locals {
            if wanted.contains(&l.mark.id) {
                continue;
            }
            let mut up = sim.parent.get(&l.mark.id).cloned().unwrap_or_default();
            let mut inside_removed = false;
            while !up.is_empty() {
                if !wanted.contains(&up) {
                    inside_removed = true;
                    break;
                }
                up = sim.parent.get(&up).cloned().unwrap_or_default();
            }
            if !inside_removed {
                ops.push(Op { op: "remove".into(), id: l.mark.id.clone(), ..Op::default() });
            }
        }
        ops
    }

    /// 내 탭을 묶어 올리고, 다른 기기의 탭이 바뀌었으면 브라우저에 넘긴다.
    fn tabs(&self, state: &mut State, read: &Read, fetched: Vec<(RemoteItem, TabsRecord)>, relay_now: u64, counts: &mut Counts, outgoing: &mut Vec<Outgoing>) {
        let mine = self.key.name_id(&tabs_name(self.device));
        for (item, record) in fetched {
            if item.id == mine {
                state.tabs_item_rev = item.rev;
                continue;
            }
            let newer = state.remote_tabs.get(&item.id).is_none_or(|(_, known)| record.at >= known.at);
            if newer {
                state.remote_tabs.insert(item.id.clone(), (item.rev, record));
            }
        }
        let due = self.now_ms.saturating_sub(state.tabs_sent_ms) >= TABS_EVERY_MS || state.tabs_sent.is_empty();
        let wanted = if self.send_tabs { read.tabs_rev.clone() } else { "off".to_string() };
        if wanted != state.tabs_sent && due && (self.send_tabs || state.tabs_item_rev > 0) {
            let mut record = TabsRecord {
                device: self.device.into(),
                name: self.device_name.into(),
                at: stamp(self.now_ms, relay_now, 0),
                workspaces: if self.send_tabs { read.tabs.clone() } else { Vec::new() },
                gone: !self.send_tabs,
            };
            loop {
                let sealed = serde_json::to_vec(&record).map_err(anyhow::Error::from).and_then(|plain| self.seal(&mine, &plain, TABS_PAD, TABS_MAX));
                match sealed {
                    Ok(blob) => {
                        outgoing.push(Outgoing { id: mine.clone(), expected_rev: state.tabs_item_rev, blob, kind: Pending::Tabs { rev: wanted } });
                        break;
                    }
                    // 너무 크면 뒤쪽 탭부터 덜어 낸다.
                    Err(_) => match record.workspaces.iter_mut().rev().find(|w| !w.tabs.is_empty()) {
                        Some(w) => {
                            let cut = w.tabs.len().div_ceil(4).max(1);
                            w.tabs.truncate(w.tabs.len() - cut);
                            w.dropped += cut;
                        }
                        None => {
                            counts.failed += 1;
                            break;
                        }
                    },
                }
            }
        }
        let mut devices: Vec<Device> = state
            .remote_tabs
            .values()
            .filter(|(_, r)| !r.gone && r.device != self.device)
            .map(|(_, r)| Device { id: r.device.clone(), name: r.name.clone(), at: r.at, workspaces: r.workspaces.clone() })
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        let digest: String = sha2::Sha256::digest(serde_json::to_vec(&devices).unwrap_or_default()).iter().take(12).map(|b| format!("{b:02x}")).collect();
        if digest != state.tabs_shown && self.browser.put_tabs(&devices).is_ok() {
            state.tabs_shown = digest;
            counts.devices_shown = devices.len();
        }
    }

    fn upload(&self, state: &mut State, outgoing: Vec<Outgoing>, key_id: &str, counts: &mut Counts) -> Result<(), SyncError> {
        let mut chunks: Vec<&[Outgoing]> = Vec::new();
        let (mut start, mut bytes) = (0, 0);
        for (i, o) in outgoing.iter().enumerate() {
            if i > start && (i - start == MAX_PUT || bytes + o.blob.len() > PUT_BYTES) {
                chunks.push(&outgoing[start..i]);
                (start, bytes) = (i, 0);
            }
            bytes += o.blob.len() + 128;
        }
        if start < outgoing.len() {
            chunks.push(&outgoing[start..]);
        }
        for chunk in chunks {
            let puts: Vec<Put> = chunk.iter().map(|o| Put { id: o.id.clone(), expected_rev: o.expected_rev, blob: Some(o.blob.clone()) }).collect();
            let sig = self.key.sign(&put_message(self.account, key_id, &puts));
            for outcome in self.remote.put(key_id, &puts, &sig)? {
                match outcome {
                    PutOutcome::Ok { id, rev, .. } => {
                        counts.uploaded += 1;
                        let Some(o) = chunk.iter().find(|o| o.id == id) else { continue };
                        match &o.kind {
                            Pending::Mark => {
                                if let Some(k) = state.marks.get_mut(&id) {
                                    k.rev = rev;
                                }
                            }
                            Pending::Tabs { rev: browser_rev } => {
                                state.tabs_item_rev = rev;
                                state.tabs_sent = browser_rev.clone();
                                state.tabs_sent_ms = self.now_ms;
                                counts.tabs_sent = true;
                            }
                        }
                    }
                    // 그사이 다른 기기가 썼다 — 다음 바퀴가 그것을 받아 다시 합친다.
                    PutOutcome::Conflict { .. } => counts.failed += 1,
                }
            }
        }
        Ok(())
    }

    /// 180일 지난 묘비를 관문에서 걷는다.
    fn sweep(&self, state: &mut State, key_id: &str) {
        let old: Vec<Put> = state
            .marks
            .iter()
            .filter(|(_, k)| {
                let r = k.record(String::new(), String::new());
                !r.alive() && k.rev > 0 && r.latest().saturating_add(TOMBSTONE_KEEP_MS) < self.now_ms
            })
            .map(|(id, k)| Put { id: id.clone(), expected_rev: k.rev, blob: None })
            .take(MAX_PUT)
            .collect();
        if old.is_empty() {
            return;
        }
        let sig = self.key.sign(&put_message(self.account, key_id, &old));
        if let Ok(outcomes) = self.remote.put(key_id, &old, &sig) {
            for outcome in outcomes {
                if let PutOutcome::Ok { id, .. } | PutOutcome::Conflict { id, current: None } = outcome {
                    state.marks.remove(&id);
                }
            }
        }
    }
}
