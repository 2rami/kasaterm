//! 두 기기와 시험용 관문으로 한 바퀴씩. 브라우저는 kasabrowser `bookmarks.Tree.Apply` 와 같은 규칙으로 흉내 낸다.
//! 모음 이름·주소는 시험용(`kasatest-…`, `kasa-test.example`)만 쓴다.

use std::cell::{Cell, RefCell};

use super::browser::{Browser, BrowserError, Collection, Device, Mark, Op, Read, Status, Tab, Workspace};
use super::engine::{Counts, Engine};
use super::state::State;
use crate::login_vault::crypto::VaultKey;
use crate::login_vault::engine::{Remote, RemoteError};
use crate::login_vault::server::{self, Fetched, Limits, Put, PutOutcome};

const ACCOUNT: &str = "acct";
const KEY: &str = "kasatest-sync@example.test";

struct Relay {
    store: server::Store,
    now: Cell<u64>,
}

impl Relay {
    fn new() -> Self {
        Self { store: server::Store::with_limits(None, Limits { items: 20_000, blob: 96 * 1024, file: 64 << 20 }), now: Cell::new(1_000_000) }
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
        self.store.fetch(ACCOUNT, since, self.now.get()).map_err(|_| RemoteError::Failed)
    }
    fn put(&self, key_id: &str, puts: &[Put], sig: &str) -> Result<Vec<PutOutcome>, RemoteError> {
        self.store.put(ACCOUNT, key_id, puts, sig, self.now.get()).map_err(|e| match e {
            server::Error::KeyMismatch => RemoteError::KeyMismatch,
            server::Error::BadSignature => RemoteError::BadSignature,
            _ => RemoteError::Failed,
        })
    }
}

/// 브라우저 흉내. 모음마다 트리와 저장 횟수, 내 탭, 받은 다른 기기 탭.
#[derive(Default)]
struct FakeBrowser {
    marks: RefCell<Vec<Mark>>,
    rev: Cell<u64>,
    tabs: RefCell<Vec<Workspace>>,
    shown: RefCell<Vec<Device>>,
    away: Cell<bool>,
    /// 다음 apply 앞에 사람이 고친 척.
    edit_before_apply: RefCell<Option<Box<dyn Fn(&mut Vec<Mark>)>>>,
}

fn locate<'a>(marks: &'a mut Vec<Mark>, id: &str) -> Option<(&'a mut Vec<Mark>, usize)> {
    if let Some(i) = marks.iter().position(|m| m.id == id) {
        return Some((marks, i));
    }
    for m in marks.iter_mut() {
        if let Some(found) = locate(&mut m.children, id) {
            return Some(found);
        }
    }
    None
}

fn contains(marks: &[Mark], id: &str) -> bool {
    marks.iter().any(|m| m.id == id || contains(&m.children, id))
}

fn children<'a>(marks: &'a mut Vec<Mark>, parent: &str) -> Result<&'a mut Vec<Mark>, String> {
    if parent.is_empty() {
        return Ok(marks);
    }
    let (list, i) = locate(marks, parent).ok_or("no parent")?;
    if !list[i].folder() {
        return Err("not a folder".into());
    }
    Ok(&mut list[i].children)
}

fn insert(list: &mut Vec<Mark>, index: i64, m: Mark) {
    let at = if index < 0 || index as usize > list.len() { list.len() } else { index as usize };
    list.insert(at, m);
}

impl FakeBrowser {
    fn op(marks: &mut Vec<Mark>, op: &Op) -> Result<(), String> {
        match op.op.as_str() {
            "add" => {
                if contains(marks, &op.id) {
                    return Err("id is taken".into());
                }
                let url = op.url.clone().unwrap_or_default();
                if op.folder == !url.is_empty() {
                    return Err("folder or address".into());
                }
                let m = Mark { id: op.id.clone(), title: op.title.clone().unwrap_or_default(), url, added: op.added, children: vec![] };
                insert(children(marks, &op.parent)?, op.index, m);
            }
            "edit" => {
                let (list, i) = locate(marks, &op.id).ok_or("no mark")?;
                if let Some(t) = &op.title {
                    list[i].title = t.clone();
                }
                if let Some(u) = &op.url {
                    list[i].url = u.clone();
                }
            }
            "move" => {
                let (list, i) = locate(marks, &op.id).ok_or("no mark")?;
                if op.parent == op.id || contains(&list[i].children, &op.parent) {
                    return Err("into itself".into());
                }
                let m = list.remove(i);
                insert(children(marks, &op.parent)?, op.index, m);
            }
            "remove" => {
                let (list, i) = locate(marks, &op.id).ok_or("no mark")?;
                list.remove(i);
            }
            _ => return Err("op".into()),
        }
        Ok(())
    }

    fn rev(&self) -> String {
        format!("e.{}", self.rev.get())
    }

    /// 사람이 고친다 — 저장 횟수가 오른다.
    fn edit(&self, change: impl FnOnce(&mut Vec<Mark>)) {
        change(&mut self.marks.borrow_mut());
        self.rev.set(self.rev.get() + 1);
    }
}

impl Browser for FakeBrowser {
    fn status(&self) -> Result<Status, BrowserError> {
        Ok(Status { rev: self.rev(), ..Status::default() })
    }
    fn read(&self) -> Result<Read, BrowserError> {
        if self.away.get() {
            return Err(BrowserError::NotRunning);
        }
        let tabs = self.tabs.borrow().clone();
        Ok(Read {
            collections: vec![Collection { key: KEY.into(), rev: self.rev(), marks: self.marks.borrow().clone() }],
            tabs_rev: format!("{:?}", tabs).len().to_string() + &tabs.iter().map(|w| w.tabs.len()).sum::<usize>().to_string(),
            tabs,
        })
    }
    fn apply(&self, collection: &str, base_rev: &str, ops: &[Op]) -> Result<String, BrowserError> {
        assert_eq!(collection, KEY);
        if let Some(edit) = self.edit_before_apply.borrow_mut().take() {
            self.edit(|m| edit(m));
        }
        if base_rev != self.rev() {
            return Err(BrowserError::Stale);
        }
        let mut next = self.marks.borrow().clone();
        for op in ops {
            Self::op(&mut next, op).map_err(|e| BrowserError::Failed(format!("{e}: {op:?}")))?;
        }
        *self.marks.borrow_mut() = next;
        self.rev.set(self.rev.get() + 1);
        Ok(self.rev())
    }
    fn put_tabs(&self, devices: &[Device]) -> Result<(), BrowserError> {
        *self.shown.borrow_mut() = devices.to_vec();
        Ok(())
    }
}

struct Side {
    id: &'static str,
    key: VaultKey,
    browser: FakeBrowser,
    state: State,
    now: u64,
    send_tabs: bool,
}

impl Side {
    fn new(id: &'static str, vault: &VaultKey) -> Self {
        let key = VaultKey::from_base64(&vault.to_base64()).unwrap().subkey("browser-sync");
        Self { id, key, browser: FakeBrowser::default(), state: State::default(), now: 1_000_000, send_tabs: true }
    }

    fn round_with(&mut self, relay: &Relay, allow_bulk_delete: bool) -> Counts {
        self.now += 60_000;
        relay.now.set(self.now);
        let engine = Engine {
            account: ACCOUNT,
            device: self.id,
            device_name: self.id,
            key: &self.key,
            browser: &self.browser,
            remote: relay,
            now_ms: self.now,
            allow_bulk_delete,
            sync_bookmarks: true,
            send_tabs: self.send_tabs,
        };
        engine.round(&mut self.state).unwrap()
    }

    fn round(&mut self, relay: &Relay) -> Counts {
        self.round_with(relay, false)
    }

    fn marks(&self) -> Vec<Mark> {
        self.browser.marks.borrow().clone()
    }
}

fn mark(id: &str, title: &str) -> Mark {
    Mark { id: id.into(), title: title.into(), url: format!("https://{id}.kasa-test.example/"), ..Mark::default() }
}

fn folder(id: &str, title: &str, children: Vec<Mark>) -> Mark {
    Mark { id: id.into(), title: title.into(), children, ..Mark::default() }
}

fn titles(marks: &[Mark]) -> Vec<String> {
    let mut out = Vec::new();
    for m in marks {
        if m.folder() {
            out.push(format!("{}[{}]", m.title, titles(&m.children).join(",")));
        } else {
            out.push(m.title.clone());
        }
    }
    out
}

/// 둘이 같아질 때까지 번갈아 돈다.
fn settle(a: &mut Side, b: &mut Side, relay: &Relay) {
    for _ in 0..3 {
        a.round(relay);
        b.round(relay);
    }
}

fn pair() -> (Side, Side, Relay) {
    let vault = VaultKey::generate().unwrap();
    (Side::new("kasatest-a", &vault), Side::new("kasatest-b", &vault), Relay::new())
}

#[test]
fn two_devices_end_with_the_same_bookmarks() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![folder("f1", "읽을 것", vec![mark("m1", "하나"), mark("m2", "둘")]), mark("m3", "셋")]);
    settle(&mut a, &mut b, &relay);
    assert_eq!(b.marks(), a.marks(), "같은 id 와 순서로");

    b.browser.edit(|m| {
        let two = m[0].children.remove(1);
        m.insert(0, two);
        m[2].title = "셋 고침".into();
    });
    settle(&mut a, &mut b, &relay);
    assert_eq!(titles(&a.marks()), vec!["둘", "읽을 것[하나]", "셋 고침"]);
    assert_eq!(a.marks(), b.marks());
}

#[test]
fn a_rename_and_a_move_made_apart_both_stay() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![folder("f1", "폴더", vec![]), mark("m1", "원래")]);
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| m[1].title = "이름 바꿈".into());
    b.browser.edit(|m| {
        let one = m.remove(1);
        m[0].children.push(one);
    });
    settle(&mut a, &mut b, &relay);
    assert_eq!(titles(&a.marks()), vec!["폴더[이름 바꿈]"]);
    assert_eq!(a.marks(), b.marks());
}

#[test]
fn a_later_edit_brings_back_what_another_device_deleted() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![mark("m1", "원래")]);
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| m.clear());
    a.round(&relay);
    b.now = a.now + 120_000;
    b.browser.edit(|m| m[0].title = "나중 고침".into());
    // b 는 a 의 지우기를 받기 전에 고쳤다 — 고침이 나중이라 되살아난다.
    settle(&mut b, &mut a, &relay);
    assert_eq!(titles(&a.marks()), vec!["나중 고침"]);
    assert_eq!(a.marks(), b.marks());
}

#[test]
fn a_mark_added_into_a_folder_deleted_elsewhere_moves_to_the_top() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![folder("f1", "폴더", vec![mark("m1", "안")])]);
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| m.clear());
    b.browser.edit(|m| m[0].children.push(mark("m2", "새것")));
    settle(&mut a, &mut b, &relay);
    assert_eq!(titles(&a.marks()), vec!["새것"], "지운 폴더의 다른 것은 같이 지워지고, 그 뒤 더한 것은 맨 위로");
    assert_eq!(a.marks(), b.marks());
}

#[test]
fn folders_moved_into_each_other_end_without_a_cycle() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![folder("x", "X", vec![]), folder("y", "Y", vec![])]);
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| {
        let x = m.remove(0);
        m[0].children.push(x);
    });
    b.now = a.now + 30_000;
    b.browser.edit(|m| {
        let y = m.remove(1);
        m[0].children.push(y);
    });
    settle(&mut a, &mut b, &relay);
    assert_eq!(a.marks(), b.marks());
    assert_eq!(a.marks().len(), 1, "하나가 다른 하나 안에");
}

#[test]
fn the_same_imported_bookmarks_are_not_doubled() {
    let (mut a, mut b, relay) = pair();
    // 같은 Chrome 북마크를 가져온 두 기기 — 주소·제목·폴더는 같고 브라우저 id 만 다르다.
    let same = |id: String, title: &str, url: &str| Mark { id, title: title.into(), url: url.into(), ..Mark::default() };
    let imported = |p: &str| {
        vec![folder(&format!("{p}f"), "가져옴", vec![
            same(format!("{p}1"), "하나", "https://one.kasa-test.example/"),
            same(format!("{p}2"), "둘", "https://two.kasa-test.example/"),
        ])]
    };
    a.browser.edit(|m| *m = imported("a"));
    b.browser.edit(|m| {
        *m = imported("b");
        m.push(mark("b3", "b 에만"));
    });
    settle(&mut a, &mut b, &relay);
    assert_eq!(titles(&a.marks()), vec!["가져옴[하나,둘]", "b 에만"]);
    assert_eq!(titles(&b.marks()), titles(&a.marks()));
}

#[test]
fn a_sudden_mass_deletion_waits_for_the_person() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = (0..40).map(|i| mark(&format!("m{i}"), &format!("{i}"))).collect());
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| m.truncate(5));
    let counts = a.round(&relay);
    assert_eq!(counts.deletions_held, 35);
    b.round(&relay);
    assert_eq!(b.marks().len(), 40, "다른 기기는 그대로");
    assert_eq!(a.marks().len(), 5, "이 기기에 되돌려 놓지도 않는다");
    a.round_with(&relay, true);
    b.round(&relay);
    assert_eq!(b.marks().len(), 5);
}

#[test]
fn an_edit_between_read_and_apply_is_kept() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = vec![mark("m1", "하나")]);
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| m.push(mark("m2", "둘")));
    a.round(&relay);
    *b.browser.edit_before_apply.borrow_mut() = Some(Box::new(|m: &mut Vec<Mark>| m.push(mark("m3", "사람이 방금"))));
    let counts = b.round(&relay);
    assert_eq!(counts.stale, 1);
    assert_eq!(titles(&b.marks()), vec!["하나", "사람이 방금"], "사람 고침을 덮지 않았다");
    settle(&mut a, &mut b, &relay);
    assert_eq!(titles(&b.marks()), titles(&a.marks()));
    assert_eq!(b.marks().len(), 3);
}

#[test]
fn a_device_shows_the_others_tabs_and_hides_them_when_turned_off() {
    let (mut a, mut b, relay) = pair();
    *a.browser.tabs.borrow_mut() = vec![Workspace {
        id: "w".into(),
        name: "개인".into(),
        tabs: vec![Tab { url: "https://tab.kasa-test.example/".into(), title: "탭".into(), ..Tab::default() }],
        ..Workspace::default()
    }];
    a.round(&relay);
    b.round(&relay);
    let shown = b.browser.shown.borrow().clone();
    assert_eq!(shown.len(), 1);
    assert_eq!((shown[0].id.as_str(), shown[0].workspaces[0].tabs[0].url.as_str()), ("kasatest-a", "https://tab.kasa-test.example/"));
    assert!(a.browser.shown.borrow().is_empty(), "제 탭은 저에게 안 보인다");
    a.send_tabs = false;
    a.round(&relay);
    b.round(&relay);
    assert!(b.browser.shown.borrow().is_empty(), "탭 보내기를 끄면 다른 기기에서도 사라진다");
}

#[test]
fn a_closed_browser_leaves_the_state_alone() {
    let (mut a, _, relay) = pair();
    a.browser.edit(|m| *m = vec![mark("m1", "하나")]);
    a.round(&relay);
    let before = a.state.clone();
    a.browser.away.set(true);
    let counts = a.round(&relay);
    assert!(counts.browser_away);
    assert_eq!(a.state, before);
}

#[test]
fn moving_one_mark_uploads_only_that_one() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = (0..10).map(|i| mark(&format!("m{i}"), &format!("{i}"))).collect());
    settle(&mut a, &mut b, &relay);
    a.browser.edit(|m| {
        let last = m.pop().unwrap();
        m.insert(0, last);
    });
    let counts = a.round(&relay);
    assert_eq!(counts.uploaded, 1, "옮긴 하나만 새 자리를 받는다");
    b.round(&relay);
    assert_eq!(titles(&b.marks())[0], "9");
}

#[test]
fn a_blob_from_another_vault_is_not_taken() {
    let (mut a, _, relay) = pair();
    a.browser.edit(|m| *m = vec![mark("m1", "하나")]);
    a.round(&relay);
    let mut stranger = Side::new("kasatest-c", &VaultKey::generate().unwrap());
    let engine = Engine {
        account: ACCOUNT,
        device: stranger.id,
        device_name: stranger.id,
        key: &stranger.key,
        browser: &stranger.browser,
        remote: &relay,
        now_ms: a.now,
        allow_bulk_delete: false,
        sync_bookmarks: true,
        send_tabs: false,
    };
    assert!(engine.round(&mut stranger.state).is_err(), "다른 열쇠는 이 관문 기록을 쓰지 못한다");
    assert!(stranger.marks().is_empty());
}

#[test]
fn many_bookmarks_go_up_in_batches_the_relay_takes() {
    let (mut a, mut b, relay) = pair();
    a.browser.edit(|m| *m = (0..450).map(|i| mark(&format!("m{i}"), &format!("{i}"))).collect());
    let counts = a.round(&relay);
    assert_eq!(counts.uploaded, 451, "북마크 450개와 내 탭 한 항목을 200개씩 나눠 올린다");
    b.round(&relay);
    assert_eq!(b.marks(), a.marks());
}
