//! 관문 쪽 보관. 잠긴 덩어리와 그 순서만 안다 — 사이트·아이디·비밀번호·바꾼 시각은 덩어리 안에 있다.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::account_sync::server::{read_account_file, write_account_file};

use super::crypto::{seal_message, signature_ok, SealedKey};

pub const MAX_ITEMS: usize = 2000;
pub const MAX_BLOB: usize = 4096;
pub const MAX_PUT: usize = 200;
const MAX_FILE: usize = MAX_ITEMS * (MAX_BLOB + 256);
/// 등록 요청은 이만큼만 산다 — 켜진 기기가 없으면 버리고 다시 한다.
pub const JOIN_TTL_MS: u64 = 10 * 60 * 1000;
const MAX_JOINS: usize = 8;
const MAX_CHALLENGE: usize = 8192;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub rev: u64,
    /// 관문이 받은 시각. 기록일 뿐 — 기기는 시각을 봉인 안에서만 읽는다.
    pub at_ms: u64,
    pub blob: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vault {
    pub key_id: String,
    /// 금고 열쇠에서 가른 쓰기 서명 공개키. 이 서명이 맞는 쓰기·봉인만 받는다 — 같은 계정이라도 열쇠 없는 기기는 못 쓴다.
    #[serde(default)]
    pub sign_pub: String,
    /// 금고를 만든 기기 이름과 때 — 새 기기의 지문 확인 화면에 보여, 모르는 기기가 먼저 만든 금고를 알아채게 한다.
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_ms: u64,
    pub revision: u64,
    pub items: BTreeMap<String, Item>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteItem {
    pub id: String,
    pub rev: u64,
    pub at_ms: u64,
    pub blob: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetched {
    /// 빈 값이면 아직 금고가 없다.
    pub key_id: String,
    pub revision: u64,
    pub items: Vec<RemoteItem>,
    /// 관문 시계. 쓰는 기기가 제 시계가 너무 앞섰는지 보고 제 시각을 깎는 데만 쓴다.
    #[serde(default)]
    pub now_ms: u64,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub created_ms: u64,
}

/// `expected_rev` 0 은 「아직 없어야 한다」. `blob` 이 없으면 걷는다(오래된 묘비).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Put {
    pub id: String,
    pub expected_rev: u64,
    pub blob: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PutOutcome {
    Ok { id: String, rev: u64, at_ms: u64 },
    Conflict { id: String, current: Option<RemoteItem> },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid(&'static str),
    /// 다른 열쇠로 쓰려 했다 — 잘못 등록된 기기다.
    KeyMismatch,
    /// 이미 금고가 있다. 만들려던 기기는 등록으로 간다.
    Exists(String),
    NotFound,
    Forbidden,
    /// 쓰기·봉인 서명이 금고 열쇠의 것이 아니다.
    BadSignature,
    /// 그 등록에는 이미 봉인이 있다 — 먼저 온 것을 덮지 않고 알린다.
    AlreadySealed,
    TooMany,
    Storage,
}

/// 쓰기 서명이 덮는 글 — 계정·열쇠 표식·항목마다 이름·기대 차례·덩어리.
pub fn put_message(account: &str, key_id: &str, puts: &[Put]) -> Vec<u8> {
    let mut text = format!("kasa.login-vault.put.v1
{account}
{key_id}
");
    for put in puts {
        text.push_str(&format!("{}
{}
{}
", put.id, put.expected_rev, put.blob.as_deref().unwrap_or("-")));
    }
    text.into_bytes()
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_key_id(key_id: &str) -> bool {
    key_id.len() == 32 && key_id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_b64(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
}

/// 새 기기가 폰 승인을 받아 붙인 것. 관문은 서명을 믿지 않고 그대로 나른다 — 확인은 열쇠를 가진 기기가 한다.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub challenge: String,
    pub key: String,
    pub sig: String,
    pub by_device: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Join {
    pub id: String,
    pub device_id: String,
    pub label: String,
    pub join_pub: String,
    pub approval: Option<Approval>,
    #[serde(skip)]
    sealed: Option<SealedKey>,
    pub created_ms: u64,
}

pub struct Store {
    dir: Option<PathBuf>,
    vaults: Mutex<HashMap<String, Vault>>,
    joins: Mutex<HashMap<String, Vec<Join>>>,
}

impl Store {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, vaults: Mutex::new(HashMap::new()), joins: Mutex::new(HashMap::new()) }
    }

    fn load(&self, account: &str, cache: &mut HashMap<String, Vault>) -> Result<Vault, Error> {
        if !kasa_identity::relay_auth::valid_account_name(account) {
            return Err(Error::Storage);
        }
        if let Some(vault) = cache.get(account) {
            return Ok(vault.clone());
        }
        let vault = match &self.dir {
            Some(dir) => match read_account_file(dir, account, MAX_FILE).map_err(|_| Error::Storage)? {
                Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?,
                None => Vault::default(),
            },
            None => Vault::default(),
        };
        cache.insert(account.into(), vault.clone());
        Ok(vault)
    }

    fn save(&self, account: &str, vault: Vault, cache: &mut HashMap<String, Vault>) -> Result<(), Error> {
        if let Some(dir) = &self.dir {
            write_account_file(dir, account, &vault).map_err(|_| Error::Storage)?;
        }
        cache.insert(account.into(), vault);
        Ok(())
    }

    pub fn init(&self, account: &str, key_id: &str, sign_pub: &str, created_by: &str, now_ms: u64) -> Result<(), Error> {
        if !valid_key_id(key_id) || !valid_b64(sign_pub, 64) {
            return Err(Error::Invalid("bad_key_id"));
        }
        let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
        let vault = self.load(account, &mut cache)?;
        if !vault.key_id.is_empty() {
            return if vault.key_id == key_id && vault.sign_pub == sign_pub { Ok(()) } else { Err(Error::Exists(vault.key_id)) };
        }
        let created_by: String = created_by.chars().filter(|c| !c.is_control()).take(64).collect();
        self.save(account, Vault { key_id: key_id.into(), sign_pub: sign_pub.into(), created_by, created_ms: now_ms, ..vault }, &mut cache)
    }

    pub fn fetch(&self, account: &str, since: u64, now_ms: u64) -> Result<Fetched, Error> {
        let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
        let vault = self.load(account, &mut cache)?;
        let items = vault
            .items
            .iter()
            .filter(|(_, item)| item.rev > since)
            .map(|(id, item)| RemoteItem { id: id.clone(), rev: item.rev, at_ms: item.at_ms, blob: item.blob.clone() })
            .collect();
        Ok(Fetched { key_id: vault.key_id, revision: vault.revision, items, now_ms, created_by: vault.created_by, created_ms: vault.created_ms })
    }

    /// 항목마다 따로 판정한다 — 하나가 부딪혀도 나머지는 들어간다.
    pub fn put(&self, account: &str, key_id: &str, puts: &[Put], sig: &str, now_ms: u64) -> Result<Vec<PutOutcome>, Error> {
        if puts.is_empty() || puts.len() > MAX_PUT {
            return Err(Error::Invalid("bad_batch"));
        }
        for put in puts {
            if !valid_id(&put.id) || put.blob.as_deref().is_some_and(|b| !valid_b64(b, MAX_BLOB)) {
                return Err(Error::Invalid("bad_item"));
            }
        }
        let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
        let mut vault = self.load(account, &mut cache)?;
        if vault.key_id.is_empty() {
            return Err(Error::NotFound);
        }
        if vault.key_id != key_id {
            return Err(Error::KeyMismatch);
        }
        if !signature_ok(&vault.sign_pub, &put_message(account, key_id, puts), sig) {
            return Err(Error::BadSignature);
        }
        let mut outcomes = Vec::with_capacity(puts.len());
        for put in puts {
            let current = vault.items.get(&put.id);
            if current.map_or(0, |item| item.rev) != put.expected_rev {
                let current = current.map(|item| RemoteItem { id: put.id.clone(), rev: item.rev, at_ms: item.at_ms, blob: item.blob.clone() });
                outcomes.push(PutOutcome::Conflict { id: put.id.clone(), current });
                continue;
            }
            match &put.blob {
                Some(blob) => {
                    if current.is_none() && vault.items.len() >= MAX_ITEMS {
                        return Err(Error::TooMany);
                    }
                    vault.revision = vault.revision.checked_add(1).ok_or(Error::Storage)?;
                    vault.items.insert(put.id.clone(), Item { rev: vault.revision, at_ms: now_ms, blob: blob.clone() });
                    outcomes.push(PutOutcome::Ok { id: put.id.clone(), rev: vault.revision, at_ms: now_ms });
                }
                None => {
                    vault.items.remove(&put.id);
                    outcomes.push(PutOutcome::Ok { id: put.id.clone(), rev: 0, at_ms: now_ms });
                }
            }
        }
        self.save(account, vault, &mut cache)?;
        Ok(outcomes)
    }

    fn live_joins<'a>(joins: &'a mut HashMap<String, Vec<Join>>, account: &str, now_ms: u64) -> &'a mut Vec<Join> {
        let list = joins.entry(account.into()).or_default();
        list.retain(|join| now_ms < join.created_ms + JOIN_TTL_MS);
        list
    }

    /// 새 기기(`device_id` 는 관문이 토큰에서 정한다)가 등록을 연다. 같은 기기의 앞 요청은 대신한다.
    pub fn open_join(&self, account: &str, device_id: &str, label: &str, join_pub: &str, now_ms: u64) -> Result<String, Error> {
        if label.chars().count() > 64 || !valid_b64(join_pub, 64) || device_id.is_empty() {
            return Err(Error::Invalid("bad_join"));
        }
        let mut joins = self.joins.lock().map_err(|_| Error::Storage)?;
        let list = Self::live_joins(&mut joins, account, now_ms);
        list.retain(|join| join.device_id != device_id);
        if list.len() >= MAX_JOINS {
            return Err(Error::TooMany);
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        list.push(Join {
            id: id.clone(),
            device_id: device_id.into(),
            label: label.into(),
            join_pub: join_pub.into(),
            approval: None,
            sealed: None,
            created_ms: now_ms,
        });
        Ok(id)
    }

    pub fn attach_approval(&self, account: &str, device_id: &str, join_id: &str, approval: Approval, now_ms: u64) -> Result<(), Error> {
        if approval.challenge.len() > MAX_CHALLENGE || approval.key.len() > 128 || approval.sig.len() > 512 || approval.by_device.len() > 128 {
            return Err(Error::Invalid("bad_approval"));
        }
        let mut joins = self.joins.lock().map_err(|_| Error::Storage)?;
        let join = Self::live_joins(&mut joins, account, now_ms).iter_mut().find(|join| join.id == join_id).ok_or(Error::NotFound)?;
        if join.device_id != device_id {
            return Err(Error::Forbidden);
        }
        join.approval = Some(approval);
        Ok(())
    }

    /// 열쇠를 가진 기기가 볼 것 — 승인이 붙었고 아직 봉인이 없는 남의 등록.
    pub fn pending_joins(&self, account: &str, device_id: &str, now_ms: u64) -> Result<Vec<Join>, Error> {
        let mut joins = self.joins.lock().map_err(|_| Error::Storage)?;
        Ok(Self::live_joins(&mut joins, account, now_ms)
            .iter()
            .filter(|join| join.device_id != device_id && join.approval.is_some() && join.sealed.is_none())
            .cloned()
            .collect())
    }

    pub fn post_sealed(&self, account: &str, device_id: &str, join_id: &str, sealed: SealedKey, now_ms: u64) -> Result<(), Error> {
        if !valid_b64(&sealed.eph_pub, 64) || !valid_b64(&sealed.blob, 256) || !valid_b64(&sealed.sig, 128) {
            return Err(Error::Invalid("bad_sealed"));
        }
        let sign_pub = {
            let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
            self.load(account, &mut cache)?.sign_pub
        };
        if !signature_ok(&sign_pub, &seal_message(account, join_id, &sealed.eph_pub, &sealed.blob), &sealed.sig) {
            return Err(Error::BadSignature);
        }
        let mut joins = self.joins.lock().map_err(|_| Error::Storage)?;
        let join = Self::live_joins(&mut joins, account, now_ms).iter_mut().find(|join| join.id == join_id).ok_or(Error::NotFound)?;
        if join.device_id == device_id || join.approval.is_none() {
            return Err(Error::Forbidden);
        }
        if join.sealed.is_some() {
            return Err(Error::AlreadySealed);
        }
        join.sealed = Some(sealed);
        Ok(())
    }

    /// 새 기기가 봉인을 받는다. 한 번 내주면 그 등록은 지운다.
    pub fn take_sealed(&self, account: &str, device_id: &str, join_id: &str, now_ms: u64) -> Result<Option<SealedKey>, Error> {
        let mut joins = self.joins.lock().map_err(|_| Error::Storage)?;
        let list = Self::live_joins(&mut joins, account, now_ms);
        let at = list.iter().position(|join| join.id == join_id).ok_or(Error::NotFound)?;
        if list[at].device_id != device_id {
            return Err(Error::Forbidden);
        }
        if list[at].sealed.is_none() {
            return Ok(None);
        }
        Ok(list.remove(at).sealed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login_vault::crypto::{seal_for, JoinSecret, VaultKey};

    fn id(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    struct Writer {
        key: VaultKey,
    }

    impl Writer {
        fn new() -> Self {
            Self { key: VaultKey::generate().unwrap() }
        }
        fn init(&self, store: &Store, account: &str) -> Result<(), Error> {
            store.init(account, &self.key.key_id(), &self.key.sign_pub(), "mac", 7)
        }
        fn put(&self, store: &Store, account: &str, puts: &[Put], now: u64) -> Result<Vec<PutOutcome>, Error> {
            let key_id = self.key.key_id();
            store.put(account, &key_id, puts, &self.key.sign(&put_message(account, &key_id, puts)), now)
        }
    }

    fn one(n: u8, expected_rev: u64, blob: Option<&str>) -> Vec<Put> {
        vec![Put { id: id(n), expected_rev, blob: blob.map(str::to_string) }]
    }

    #[test]
    fn first_init_wins_and_writes_need_the_vault_signature() {
        let store = Store::new(None);
        let owner = Writer::new();
        assert_eq!(owner.put(&store, "alice", &one(1, 0, Some("QQ==")), 1), Err(Error::NotFound));
        owner.init(&store, "alice").unwrap();
        owner.init(&store, "alice").unwrap();
        let other = Writer::new();
        assert_eq!(other.init(&store, "alice"), Err(Error::Exists(owner.key.key_id())));
        assert_eq!(other.put(&store, "alice", &one(1, 0, Some("QQ==")), 1), Err(Error::KeyMismatch));
        // 열쇠 표식은 누구나 받아 볼 수 있다 — 표식만 맞춘 쓰기는 서명에서 막힌다.
        let key_id = owner.key.key_id();
        let forged = other.key.sign(&put_message("alice", &key_id, &one(1, 0, Some("QQ=="))));
        assert_eq!(store.put("alice", &key_id, &one(1, 0, Some("QQ==")), &forged, 1), Err(Error::BadSignature));
        let signed = owner.key.sign(&put_message("alice", &key_id, &one(1, 0, Some("QQ=="))));
        assert_eq!(store.put("alice", &key_id, &one(1, 0, Some("Qg==")), &signed, 1), Err(Error::BadSignature), "서명은 덩어리까지 덮는다");
        assert_eq!(store.fetch("bob", 0, 5).unwrap().key_id, "", "계정끼리 섞이지 않는다");
        let fetched = store.fetch("alice", 0, 5).unwrap();
        assert_eq!(fetched.now_ms, 5, "관문 시계를 함께 준다");
        assert_eq!((fetched.created_by.as_str(), fetched.created_ms), ("mac", 7), "금고를 만든 기기와 때");
    }

    #[test]
    fn each_item_is_checked_on_its_own_revision() {
        let store = Store::new(None);
        let w = Writer::new();
        w.init(&store, "alice").unwrap();
        let out = w.put(&store, "alice", &one(1, 0, Some("QQ==")), 100).unwrap();
        assert_eq!(out, vec![PutOutcome::Ok { id: id(1), rev: 1, at_ms: 100 }]);
        let batch = vec![
            Put { id: id(1), expected_rev: 0, blob: Some("Qg==".into()) },
            Put { id: id(2), expected_rev: 0, blob: Some("Qw==".into()) },
        ];
        let out = w.put(&store, "alice", &batch, 200).unwrap();
        assert!(matches!(&out[0], PutOutcome::Conflict { current: Some(c), .. } if c.rev == 1 && c.blob == "QQ=="));
        assert_eq!(out[1], PutOutcome::Ok { id: id(2), rev: 2, at_ms: 200 });
        let fetched = store.fetch("alice", 1, 0).unwrap();
        assert_eq!((fetched.revision, fetched.items.len()), (2, 1));
        w.put(&store, "alice", &one(1, 1, None), 300).unwrap();
        assert_eq!(store.fetch("alice", 0, 0).unwrap().items.len(), 1, "걷은 묘비는 사라진다");
        assert!(w.put(&store, "alice", &[Put { id: "zz".into(), expected_rev: 0, blob: None }], 1).is_err());
    }

    #[test]
    fn vault_survives_restart_and_corrupt_files_are_not_replaced() {
        let dir = std::env::temp_dir().join(format!("kasa-login-vault-{}", uuid::Uuid::new_v4()));
        let store = Store::new(Some(dir.clone()));
        let w = Writer::new();
        w.init(&store, "alice").unwrap();
        w.put(&store, "alice", &one(1, 0, Some("QQ==")), 5).unwrap();
        let again = Store::new(Some(dir.clone()));
        assert_eq!(again.fetch("alice", 0, 0).unwrap().items.len(), 1);
        assert!(w.put(&again, "alice", &one(1, 1, Some("Qg==")), 6).is_ok(), "서명 공개키도 남는다");
        kasa_identity::relay_auth::write_private(&dir.join("bob.json"), "broken").unwrap();
        assert_eq!(w.init(&Store::new(Some(dir.clone())), "bob"), Err(Error::Storage));
        assert_eq!(std::fs::read_to_string(dir.join("bob.json")).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn joins_flow_between_the_new_device_and_a_holder_once() {
        let store = Store::new(None);
        let w = Writer::new();
        w.init(&store, "alice").unwrap();
        let secret = JoinSecret::new().unwrap();
        let join = store.open_join("alice", "new", "새 PC", &secret.public_base64(), 0).unwrap();
        assert!(store.pending_joins("alice", "old", 1).unwrap().is_empty(), "승인 전에는 안 보인다");
        let approval = Approval { challenge: "{}".into(), key: "k".into(), sig: "s".into(), by_device: "phone".into() };
        assert_eq!(store.attach_approval("alice", "old", &join, approval.clone(), 1), Err(Error::Forbidden));
        store.attach_approval("alice", "new", &join, approval, 1).unwrap();
        assert!(store.pending_joins("alice", "new", 2).unwrap().is_empty(), "제 요청은 제가 봉인하지 않는다");
        assert_eq!(store.pending_joins("alice", "old", 2).unwrap().len(), 1);
        let sealed = seal_for(&w.key, "alice", "new", &join, &secret.public_base64()).unwrap();
        assert_eq!(store.post_sealed("alice", "new", &join, sealed.clone(), 3), Err(Error::Forbidden));
        let stranger = seal_for(&VaultKey::generate().unwrap(), "alice", "new", &join, &secret.public_base64()).unwrap();
        assert_eq!(store.post_sealed("alice", "old", &join, stranger, 3), Err(Error::BadSignature), "열쇠 없는 기기는 봉인을 못 올린다");
        store.post_sealed("alice", "old", &join, sealed.clone(), 3).unwrap();
        assert_eq!(store.post_sealed("alice", "old", &join, sealed.clone(), 3), Err(Error::AlreadySealed));
        assert_eq!(store.take_sealed("alice", "old", &join, 4), Err(Error::Forbidden));
        assert_eq!(store.take_sealed("alice", "new", &join, 4).unwrap(), Some(sealed));
        assert_eq!(store.take_sealed("alice", "new", &join, 5), Err(Error::NotFound), "한 번만 내준다");
        let late = store.open_join("alice", "new", "새 PC", &secret.public_base64(), 10).unwrap();
        assert_eq!(store.take_sealed("alice", "new", &late, 10 + JOIN_TTL_MS), Err(Error::NotFound), "10분 지나면 버린다");
    }
}
