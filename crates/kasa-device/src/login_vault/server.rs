//! 관문 쪽 보관. 잠긴 덩어리와 그 순서만 안다 — 사이트·아이디·비밀번호·바꾼 시각은 덩어리 안에 있다.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::account_sync::server::{read_account_file, write_account_file};

use super::crypto::{signature_ok, VaultKey};

pub const MAX_ITEMS: usize = 2000;
pub const MAX_BLOB: usize = 4096;
pub const MAX_PUT: usize = 200;
const MAX_FILE: usize = MAX_ITEMS * (MAX_BLOB + 256);

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
    /// 맡긴 금고 열쇠(base64). 같은 계정으로 로그인한 데스크톱이면 받아 간다 — 관문이 금고를 열 수 있는 대신 새 기기는
    /// 로그인만으로 맞추기 시작한다(docs/login-vault-sync.md 「위협 모델」).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub escrow: String,
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
    /// 이미 금고가 있다. 만들려던 기기는 맡긴 열쇠를 받는다.
    Exists(String),
    NotFound,
    /// 쓰기 서명이 금고 열쇠의 것이 아니다.
    BadSignature,
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

/// 한 금고의 한도. 로그인 금고는 [`Limits::LOGINS`], 북마크·탭 동기화는 제 것을 쓴다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub items: usize,
    pub blob: usize,
    /// 파일 하나(계정 하나)의 한도 — 덩어리 크기가 들쭉날쭉한 쓰임도 읽지 못할 만큼 커지지 않게.
    pub file: usize,
}

impl Limits {
    pub const LOGINS: Limits = Limits { items: MAX_ITEMS, blob: MAX_BLOB, file: MAX_FILE };
}

pub struct Store {
    dir: Option<PathBuf>,
    limits: Limits,
    vaults: Mutex<HashMap<String, Vault>>,
}

impl Store {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self::with_limits(dir, Limits::LOGINS)
    }

    pub fn with_limits(dir: Option<PathBuf>, limits: Limits) -> Self {
        Self { dir, limits, vaults: Mutex::new(HashMap::new()) }
    }

    fn load(&self, account: &str, cache: &mut HashMap<String, Vault>) -> Result<Vault, Error> {
        if !kasa_identity::relay_auth::valid_account_name(account) {
            return Err(Error::Storage);
        }
        if let Some(vault) = cache.get(account) {
            return Ok(vault.clone());
        }
        let vault = match &self.dir {
            Some(dir) => match read_account_file(dir, account, self.limits.file).map_err(|_| Error::Storage)? {
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
            if !valid_id(&put.id) || put.blob.as_deref().is_some_and(|b| !valid_b64(b, self.limits.blob)) {
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
        let size = |item: &Item| item.blob.len() + 128;
        let mut total: usize = vault.items.values().map(size).sum();
        for put in puts {
            let current = vault.items.get(&put.id);
            if current.map_or(0, |item| item.rev) != put.expected_rev {
                let current = current.map(|item| RemoteItem { id: put.id.clone(), rev: item.rev, at_ms: item.at_ms, blob: item.blob.clone() });
                outcomes.push(PutOutcome::Conflict { id: put.id.clone(), current });
                continue;
            }
            match &put.blob {
                Some(blob) => {
                    if current.is_none() && vault.items.len() >= self.limits.items {
                        return Err(Error::TooMany);
                    }
                    total = total - current.map_or(0, size) + blob.len() + 128;
                    // 머리(열쇠 표식·서명 공개키·차례)를 남긴 자리. 로그인 금고는 항목 수 한도가 먼저 걸린다.
                    if total > self.limits.file.saturating_sub(4096) {
                        return Err(Error::TooMany);
                    }
                    vault.revision = vault.revision.checked_add(1).ok_or(Error::Storage)?;
                    vault.items.insert(put.id.clone(), Item { rev: vault.revision, at_ms: now_ms, blob: blob.clone() });
                    outcomes.push(PutOutcome::Ok { id: put.id.clone(), rev: vault.revision, at_ms: now_ms });
                }
                None => {
                    total -= current.map_or(0, size);
                    vault.items.remove(&put.id);
                    outcomes.push(PutOutcome::Ok { id: put.id.clone(), rev: 0, at_ms: now_ms });
                }
            }
        }
        self.save(account, vault, &mut cache)?;
        Ok(outcomes)
    }

    /// 열쇠를 가진 기기가 금고 열쇠를 맡긴다. 관문이 열쇠에서 표식·서명 공개키를 다시 만들어 금고와 같을 때만 받는다 —
    /// 열쇠 없는 기기가 다른 열쇠를 끼워 새 기기들을 엉뚱한 열쇠로 돌게 하지 못하게.
    pub fn put_escrow(&self, account: &str, key: &str) -> Result<(), Error> {
        if !valid_b64(key, 64) {
            return Err(Error::Invalid("bad_key"));
        }
        let vault_key = VaultKey::from_base64(key).map_err(|_| Error::Invalid("bad_key"))?;
        let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
        let vault = self.load(account, &mut cache)?;
        if vault.key_id.is_empty() {
            return Err(Error::NotFound);
        }
        if vault_key.key_id() != vault.key_id || vault_key.sign_pub() != vault.sign_pub {
            return Err(Error::KeyMismatch);
        }
        if vault.escrow == key {
            return Ok(());
        }
        self.save(account, Vault { escrow: key.into(), ..vault }, &mut cache)
    }

    /// 맡긴 금고 열쇠. 금고가 없거나 아직 아무도 맡기지 않았으면 None.
    pub fn escrow(&self, account: &str) -> Result<Option<String>, Error> {
        let mut cache = self.vaults.lock().map_err(|_| Error::Storage)?;
        let vault = self.load(account, &mut cache)?;
        Ok(Some(vault.escrow).filter(|key| !key.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login_vault::crypto::VaultKey;

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
    fn only_the_vaults_own_key_can_be_left_and_any_desktop_takes_it_back() {
        let store = Store::new(None);
        let w = Writer::new();
        let key = w.key.to_base64().to_string();
        assert_eq!(store.put_escrow("alice", &key), Err(Error::NotFound), "금고가 없으면 맡길 곳이 없다");
        w.init(&store, "alice").unwrap();
        assert_eq!(store.escrow("alice").unwrap(), None);
        let stranger = VaultKey::generate().unwrap().to_base64().to_string();
        assert_eq!(store.put_escrow("alice", &stranger), Err(Error::KeyMismatch), "열쇠 없는 기기는 다른 열쇠를 못 끼운다");
        assert_eq!(store.put_escrow("alice", "not a key"), Err(Error::Invalid("bad_key")));
        store.put_escrow("alice", &key).unwrap();
        store.put_escrow("alice", &key).unwrap();
        assert_eq!(store.escrow("alice").unwrap(), Some(key.clone()));
        assert_eq!(store.escrow("bob").unwrap(), None, "다른 계정의 것은 없다");
        let fetched = store.fetch("alice", 0, 1).unwrap();
        assert!(!serde_json::to_string(&fetched).unwrap().contains(&key), "항목 받기에는 열쇠가 섞이지 않는다");
    }
}
