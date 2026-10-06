//! 이 기계의 영구 id. 공용 주소 안에서 같은 표시 이름의 기계를 가른다. 본판과 `kasa tui` 가
//! 같은 파일(`~/.config/kasaterm/machine-id`)을 읽어 한 기계로 보인다.

use std::path::PathBuf;
use std::sync::Mutex;

static WRITE: Mutex<()> = Mutex::new(());

pub fn machine_identity_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASATERM_MACHINE_ID_FILE") {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("KASATERM_MOBILE_USERS") {
        return Some(PathBuf::from(path).with_extension("machine-id"));
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/machine-id"))
}

fn valid_machine_identity(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// 공용 주소 안에서 같은 표시 이름의 기계를 가르는 로컬 영구 id.
pub fn machine_identity() -> Option<String> {
    if let Ok(value) = std::env::var("KASATERM_MACHINE_ID") {
        let value = value.trim();
        return valid_machine_identity(value).then(|| value.to_string());
    }
    let path = machine_identity_path()?;
    let _guard = WRITE.lock().ok()?;
    if let Ok(value) = std::fs::read_to_string(&path) {
        let value = value.trim();
        if valid_machine_identity(value) {
            return Some(value.to_string());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let value = uuid::Uuid::new_v4().simple().to_string();
    let wrote = {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut file| file.write_all(value.as_bytes()))
            .is_ok()
    };
    if !wrote {
        let existing = std::fs::read_to_string(&path).ok()?;
        let existing = existing.trim();
        return valid_machine_identity(existing).then(|| existing.to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Some(value)
}
