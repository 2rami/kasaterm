//! Windows 자격 증명 관리자의 로그인 항목. 자리는 브라우저 규칙 그대로다(kasabrowser-go docs/kasa-keychain-logins.md):
//! 대상 `web:<host>` 가 비었거나 같은 아이디(대소문자 무시)면 거기, 아니면 `<아이디>@web:<host>`.

use zeroize::Zeroizing;

use crate::wincred;

use super::store::{parse_value, storable, value_json, Entry, LoginStore};

pub struct WindowsStore {
    prefix: String,
}

impl WindowsStore {
    pub fn new(prefix: &str) -> Self {
        Self { prefix: prefix.into() }
    }

    fn base(&self, host: &str) -> String {
        format!("{}{host}", self.prefix)
    }

    /// 대상 이름을 `(host, 그 대상의 아이디)` 로 읽는다. 둘째 계정 꼴의 아이디는 이메일처럼 `@` 를 품을 수 있어 뒤에서 가른다.
    fn parse_target(&self, target: &str, user: &str) -> Option<(String, String)> {
        if let Some(host) = target.strip_prefix(&self.prefix) {
            return Some((host.to_string(), user.to_string()));
        }
        let marker = format!("@{}", self.prefix);
        let at = target.rfind(&marker)?;
        let host = &target[at + marker.len()..];
        Some((host.to_string(), user.to_string()))
    }

    /// 그 로그인이 있는 대상 이름.
    fn find(&self, host: &str, username: &str) -> anyhow::Result<Option<String>> {
        let wanted = username.to_lowercase();
        if let Some(cred) = wincred::read(&self.base(host))? {
            if cred.user.to_lowercase() == wanted {
                return Ok(Some(cred.target));
            }
        }
        if let Some(cred) = wincred::read(&format!("{username}@{}", self.base(host)))? {
            return Ok(Some(cred.target));
        }
        // 철자만 다른 둘째 계정 자리.
        let suffix = format!("@{}", self.base(host));
        Ok(wincred::enumerate(|target| target.ends_with(&suffix))?
            .into_iter()
            .find(|c| c.user.to_lowercase() == wanted)
            .map(|c| c.target))
    }
}

impl LoginStore for WindowsStore {
    fn list(&self) -> anyhow::Result<Vec<Entry>> {
        Ok(wincred::enumerate(|target| self.parse_target(target, "").is_some())?
            .into_iter()
            .filter_map(|c| {
                let (host, username) = self.parse_target(&c.target, &c.user)?;
                Some(Entry { host, username, modified_ms: Some(c.written_ms) })
            })
            .collect())
    }

    fn read(&self, host: &str, username: &str) -> anyhow::Result<Option<Zeroizing<String>>> {
        let Some(target) = self.find(host, username)? else { return Ok(None) };
        let Some(cred) = wincred::read(&target)? else { return Ok(None) };
        let raw = Zeroizing::new(String::from_utf8(wincred::decode(cred.blob)).unwrap_or_default());
        Ok(Some(parse_value(&raw, &cred.user).1))
    }

    fn write(&self, host: &str, username: &str, password: &str) -> anyhow::Result<()> {
        anyhow::ensure!(storable(host, username), "not_storable");
        let value = value_json(username, password);
        let base = self.base(host);
        let target = match self.find(host, username)? {
            // 대상 이름은 대소문자를 안 가려, 철자가 바뀐 둘째 계정도 같은 자리에 쓰면 사용자 칸만 새 철자가 된다.
            Some(target) => target,
            None if wincred::read(&base)?.is_none() => base,
            None => format!("{username}@{base}"),
        };
        wincred::write(&target, username, value.as_bytes(), "")
    }

    fn delete(&self, host: &str, username: &str) -> anyhow::Result<()> {
        if let Some(target) = self.find(host, username)? {
            wincred::delete(&target);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::TEST_PREFIX;
    use super::*;

    /// 진짜 자격 증명 관리자를 시험 접두사로만 쓴다 — 진짜 `web:` 항목은 건드리지 않는다. 손으로 돌린다:
    /// `cargo test -p kasa-device --lib windows_store -- --ignored`
    #[test]
    #[ignore = "자격 증명 관리자에 시험 항목을 썼다 지운다 — 손으로 돌린다"]
    fn browser_placement_rules_with_the_test_prefix() {
        let store = WindowsStore::new(TEST_PREFIX);
        let host = format!("probe-{}.example", std::process::id());
        // 중간 단언이 실패해도 시험 항목을 남기지 않는다.
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for c in wincred::enumerate(|target| target.ends_with(&self.0)).unwrap_or_default() {
                    wincred::delete(&c.target);
                }
            }
        }
        let cleanup = Cleanup(format!("{TEST_PREFIX}{host}"));
        drop(Cleanup(cleanup.0.clone()));
        store.write(&host, "First@Example.com", "p1").unwrap();
        store.write(&host, "second", "p2").unwrap();
        assert!(wincred::read(&format!("{TEST_PREFIX}{host}")).unwrap().is_some(), "첫 계정은 web:<host>");
        assert!(wincred::read(&format!("second@{TEST_PREFIX}{host}")).unwrap().is_some(), "둘째는 <아이디>@web:<host>");
        let mut listed: Vec<_> = store.list().unwrap().into_iter().filter(|e| e.host == host).map(|e| e.username).collect();
        listed.sort();
        assert_eq!(listed, vec!["First@Example.com".to_string(), "second".to_string()]);
        assert_eq!(store.read(&host, "first@example.com").unwrap().as_deref().map(String::as_str), Some("p1"), "아이디는 대소문자를 가리지 않는다");
        store.write(&host, "SECOND", "p3").unwrap();
        assert_eq!(store.read(&host, "second").unwrap().as_deref().map(String::as_str), Some("p3"));
        let spelled: Vec<_> = store.list().unwrap().into_iter().filter(|e| e.host == host).map(|e| e.username).collect();
        assert!(spelled.contains(&"SECOND".to_string()) && spelled.len() == 2, "철자만 바뀐 아이디는 같은 자리에서 새 철자가 된다");
        store.delete(&host, "first@example.com").unwrap();
        assert_eq!(store.read(&host, "First@Example.com").unwrap(), None);
        assert!(store.list().unwrap().iter().all(|e| !(e.host == host && e.username.starts_with("First"))));
        drop(cleanup);
        assert!(store.list().unwrap().iter().all(|e| e.host != host));
        assert!(store.list().unwrap().iter().all(|e| !e.host.is_empty()));
    }
}
