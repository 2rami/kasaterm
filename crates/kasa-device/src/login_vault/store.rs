//! 로컬 키체인 쪽. 브라우저가 읽는 바로 그 항목을 맞춘다 — 맥 일반 암호 `web:<host>`·계정 = 아이디,
//! Windows 대상 `web:<host>`·`<아이디>@web:<host>`. 값은 `{"username","password"}` JSON.

use std::collections::BTreeMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

/// 진짜 항목의 서비스 접두사. 시험은 [`TEST_PREFIX`] 만 쓴다.
pub const PREFIX: &str = "web:";
pub const TEST_PREFIX: &str = "kasatest-web:";

/// 목록 한 줄 — 값은 없다(맥은 속성만 읽으면 허용 창이 안 뜬다).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub host: String,
    pub username: String,
    /// 마지막으로 바뀐 때. 같으면 값을 다시 읽지 않는다.
    pub modified_ms: Option<u64>,
}

/// 지우기를 키체인이 거절했다 — 맥에서 브라우저가 만든 항목은 소유자(브라우저)만 지운다.
pub const OWNER_EDIT: &str = "owner_edit";

pub trait LoginStore {
    fn list(&self) -> anyhow::Result<Vec<Entry>>;
    /// 비밀번호. 항목이 없으면 `None`. 놓을 때 지워지는 그릇에 담는다.
    fn read(&self, host: &str, username: &str) -> anyhow::Result<Option<Zeroizing<String>>>;
    /// 같은 사이트에 아이디가 대소문자만 다른 항목이 있으면 그 자리를 고치고 철자도 맞춘다(브라우저의 「바꾸기」와 같다).
    fn write(&self, host: &str, username: &str, password: &str) -> anyhow::Result<()>;
    /// 지울 수 없으면 [`OWNER_EDIT`] 오류.
    fn delete(&self, host: &str, username: &str) -> anyhow::Result<()>;
}

/// 값 JSON 을 읽는다. JSON 이 아니면 통째로 비밀번호이고 아이디는 항목의 계정이다(브라우저와 같은 규칙).
pub fn parse_value(raw: &str, account: &str) -> (String, Zeroizing<String>) {
    #[derive(serde::Deserialize)]
    struct Value {
        username: Option<String>,
        password: Zeroizing<String>,
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => (v.username.unwrap_or_else(|| account.to_string()), v.password),
        Err(_) => (account.to_string(), Zeroizing::new(raw.to_string())),
    }
}

pub fn value_json(username: &str, password: &str) -> Zeroizing<String> {
    Zeroizing::new(serde_json::json!({"username": username, "password": password}).to_string())
}

/// 아이디에 제어 문자가 있으면 두 OS 모두 저장하지 않는다(브라우저 규칙).
pub fn storable(host: &str, username: &str) -> bool {
    !host.is_empty() && !host.chars().any(char::is_control) && !username.chars().any(char::is_control)
}

/// 시험·검증 리그용 — 진짜 키체인을 안 건드린다.
#[derive(Default)]
pub struct MemoryStore {
    items: Mutex<BTreeMap<(String, String), (String, u64)>>,
    clock: Mutex<u64>,
    /// 이 사이트의 항목은 지우기를 거절한다(맥의 브라우저 소유 항목 흉내).
    owned_by_other: Mutex<Vec<String>>,
    /// 쓰기를 실패시킨다(잠긴 키체인·허용 창 거절 흉내).
    fail_writes: Mutex<bool>,
    /// 값 읽기를 실패시킨다(목록은 그대로).
    fail_reads: Mutex<bool>,
}

impl MemoryStore {
    fn tick(&self) -> u64 {
        let mut clock = self.clock.lock().unwrap();
        *clock += 1;
        *clock
    }

    /// 다음 쓰기의 수정 시각을 이 값 다음으로 — 시험이 「언제 바꿨나」를 정한다.
    pub fn set_clock(&self, ms: u64) {
        *self.clock.lock().unwrap() = ms;
    }

    pub fn fail_writes(&self, fail: bool) {
        *self.fail_writes.lock().unwrap() = fail;
    }

    pub fn fail_reads(&self, fail: bool) {
        *self.fail_reads.lock().unwrap() = fail;
    }

    pub fn refuse_delete_for(&self, host: &str) {
        self.owned_by_other.lock().unwrap().push(host.into());
    }

    pub fn snapshot(&self) -> BTreeMap<(String, String), String> {
        self.items.lock().unwrap().iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect()
    }

    fn find(items: &BTreeMap<(String, String), (String, u64)>, host: &str, username: &str) -> Option<(String, String)> {
        let exact = (host.to_string(), username.to_string());
        if items.contains_key(&exact) {
            return Some(exact);
        }
        items.keys().find(|(h, u)| h == host && u.to_lowercase() == username.to_lowercase()).cloned()
    }
}

impl LoginStore for MemoryStore {
    fn list(&self) -> anyhow::Result<Vec<Entry>> {
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|((host, username), (_, at))| Entry { host: host.clone(), username: username.clone(), modified_ms: Some(*at) })
            .collect())
    }

    fn read(&self, host: &str, username: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
        anyhow::ensure!(!*self.fail_reads.lock().unwrap(), "keychain_locked");
        let items = self.items.lock().unwrap();
        Ok(Self::find(&items, host, username).and_then(|k| items.get(&k).map(|(v, _)| Zeroizing::new(v.clone()))))
    }

    fn write(&self, host: &str, username: &str, password: &str) -> anyhow::Result<()> {
        anyhow::ensure!(storable(host, username), "not_storable");
        anyhow::ensure!(!*self.fail_writes.lock().unwrap(), "keychain_locked");
        let at = self.tick();
        let mut items = self.items.lock().unwrap();
        if let Some(old) = Self::find(&items, host, username) {
            items.remove(&old);
        }
        items.insert((host.into(), username.into()), (password.into(), at));
        Ok(())
    }

    fn delete(&self, host: &str, username: &str) -> anyhow::Result<()> {
        if self.owned_by_other.lock().unwrap().iter().any(|h| h == host) {
            anyhow::bail!(OWNER_EDIT);
        }
        let mut items = self.items.lock().unwrap();
        if let Some(k) = Self::find(&items, host, username) {
            items.remove(&k);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_follow_the_browser_format() {
        let pair = |(u, p): (String, Zeroizing<String>)| (u, p.to_string());
        assert_eq!(pair(parse_value(r#"{"username":"a@x","password":"p"}"#, "acct")), ("a@x".into(), "p".into()));
        assert_eq!(pair(parse_value(r#"{"password":"p"}"#, "acct")), ("acct".into(), "p".into()));
        assert_eq!(pair(parse_value("plain", "acct")), ("acct".into(), "plain".into()));
        assert_eq!(pair(parse_value(&value_json("u", "p\"q"), "")), ("u".into(), "p\"q".into()));
        assert!(!storable("x.com", "a\nb"));
        assert!(!storable("", "a"));
    }

    #[test]
    fn memory_store_replaces_case_variants_in_place() {
        let store = MemoryStore::default();
        store.write("x.com", "Alice", "1").unwrap();
        store.write("x.com", "alice", "2").unwrap();
        assert_eq!(store.snapshot().len(), 1);
        assert_eq!(store.read("x.com", "ALICE").unwrap().as_deref().map(String::as_str), Some("2"));
        store.refuse_delete_for("x.com");
        assert_eq!(store.delete("x.com", "alice").unwrap_err().to_string(), OWNER_EDIT);
    }
}
