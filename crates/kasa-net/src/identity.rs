//! 기기 신원 키. 비밀키 32바이트를 그대로 파일에 두고 0600 으로 잠근다.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use iroh::SecretKey;

pub const KEY_FILE: &str = "kasanet.key";
/// 격리 인스턴스(검증 앱·시험)가 본판과 같은 키로 뜨면 상대가 두 기기를 한 기기로 본다 — 경로를 가를 창구.
pub const KEY_PATH_ENV: &str = "KASATERM_KASANET_KEY";

pub fn default_key_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(KEY_PATH_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config/kasaterm").join(KEY_FILE))
}

/// 있으면 읽고, 없으면 만들어 둔다. 두 프로세스가 동시에 만들어도 먼저 걸린 키 하나로 모인다.
pub fn load_or_create(path: &Path) -> io::Result<SecretKey> {
    match read(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        other => return other,
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let key = SecretKey::generate();
    // 임시 파일을 다 쓴 뒤 hard_link 로 붙인다 — 이름이 이미 있으면 실패하므로 반쯤 쓴 키를 남이 읽는 틈이 없다.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    {
        let mut f = private_create(&tmp)?;
        f.write_all(&key.to_bytes())?;
        f.sync_all()?;
    }
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read(path),
        Err(e) => Err(e),
    }
}

fn read(path: &Path) -> io::Result<SecretKey> {
    let bytes = fs::read(path)?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{}: 신원 키는 32바이트여야 한다({}바이트)",
                path.display(),
                bytes.len()
            ),
        )
    })?;
    tighten(path)?;
    Ok(SecretKey::from_bytes(&arr))
}

#[cfg(unix)]
fn private_create(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_create(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn tighten(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn tighten(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_reloads_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(KEY_FILE);
        let a = load_or_create(&path).unwrap();
        let b = load_or_create(&path).unwrap();
        assert_eq!(a.public(), b.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "임시 파일이 남으면 안 된다");
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(KEY_FILE);
        let key = load_or_create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load_or_create(&path).unwrap().public(), key.public());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn corrupt_key_is_an_error_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(KEY_FILE);
        fs::write(&path, b"short").unwrap();
        assert_eq!(
            load_or_create(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), b"short");
    }
}
