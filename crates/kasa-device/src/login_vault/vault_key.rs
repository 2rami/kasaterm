//! 이 기기의 금고 열쇠 보관. 맥은 일반 암호 서비스 `kasaterm.login-vault`(kasaterm 이 만들어 기본 접근 목록이
//! kasaterm 하나다), Windows 는 대상 `kasaterm.login-vault/<계정>`. 파일·로그에는 쓰지 않는다.
//! 격리 실행(검증 리그)은 사람의 키체인을 안 건드린다 — `KASATERM_LOGIN_VAULT_DIR` 가 준 폴더만 쓴다.

use std::path::PathBuf;

use super::crypto::VaultKey;

const SERVICE: &str = "kasaterm.login-vault";

fn isolated_dir() -> anyhow::Result<Option<PathBuf>> {
    if crate::device_auth::sync_environment_allowed() {
        return Ok(None);
    }
    match std::env::var_os("KASATERM_LOGIN_VAULT_DIR") {
        Some(dir) => Ok(Some(PathBuf::from(dir))),
        None => anyhow::bail!("isolated_run"),
    }
}

pub fn load(account_key: &str) -> anyhow::Result<Option<VaultKey>> {
    let text = match isolated_dir()? {
        Some(dir) => std::fs::read_to_string(dir.join(account_key)).ok(),
        None => platform::get(account_key)?,
    };
    text.map(|t| VaultKey::from_base64(&t)).transpose()
}

pub fn save(account_key: &str, key: &VaultKey) -> anyhow::Result<()> {
    match isolated_dir()? {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            Ok(kasa_identity::relay_auth::write_private(&dir.join(account_key), &key.to_base64())?)
        }
        None => platform::set(account_key, &key.to_base64()),
    }
}

/// 관문에 금고를 못 만들었을 때 방금 둔 열쇠를 거둔다.
pub fn forget(account_key: &str) -> anyhow::Result<()> {
    match isolated_dir()? {
        Some(dir) => {
            let _ = std::fs::remove_file(dir.join(account_key));
            Ok(())
        }
        None => platform::delete(account_key),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use security_framework::passwords;

    const NOT_FOUND: i32 = -25300;

    pub fn delete(account: &str) -> anyhow::Result<()> {
        match passwords::delete_generic_password(super::SERVICE, account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == NOT_FOUND => Ok(()),
            Err(error) => anyhow::bail!("keychain: {}", error.code()),
        }
    }

    pub fn get(account: &str) -> anyhow::Result<Option<String>> {
        match passwords::get_generic_password(super::SERVICE, account) {
            Ok(bytes) => Ok(Some(String::from_utf8(bytes)?)),
            Err(error) if error.code() == NOT_FOUND => Ok(None),
            Err(error) => anyhow::bail!("keychain: {}", error.code()),
        }
    }

    pub fn set(account: &str, value: &str) -> anyhow::Result<()> {
        passwords::set_generic_password(super::SERVICE, account, value.as_bytes())
            .map_err(|error| anyhow::anyhow!("keychain: {}", error.code()))
    }
}

#[cfg(windows)]
mod platform {
    use crate::wincred;

    fn target(account: &str) -> String {
        format!("{}/{account}", super::SERVICE)
    }

    pub fn get(account: &str) -> anyhow::Result<Option<String>> {
        Ok(wincred::read(&target(account))?.map(|c| String::from_utf8_lossy(&wincred::decode(c.blob)).into_owned()))
    }

    pub fn set(account: &str, value: &str) -> anyhow::Result<()> {
        wincred::write(&target(account), "kasaterm", value.as_bytes(), "")
    }

    pub fn delete(account: &str) -> anyhow::Result<()> {
        wincred::delete(&target(account));
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    pub fn get(_: &str) -> anyhow::Result<Option<String>> {
        anyhow::bail!("unsupported")
    }
    pub fn set(_: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("unsupported")
    }
    pub fn delete(_: &str) -> anyhow::Result<()> {
        anyhow::bail!("unsupported")
    }
}
