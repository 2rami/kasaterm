//! Per-account sealed files on the gateway: AES-256-GCM under a master key that lives in the same
//! private directory, with the domain and account name bound as associated data. Each store keeps
//! its own directory and key, so one leaked key does not open another store.

use base64::Engine as _;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::File;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_ENVELOPE: u64 = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Storage,
}
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) struct Vault {
    directory: PathBuf,
    domain: &'static str,
    key: aead::LessSafeKey,
    _lock: File,
    #[cfg(windows)]
    _directory: kasa_socket::private_store::DirGuard,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    nonce: String,
    ciphertext: String,
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Storage)?;
    Ok(bytes)
}

fn read_private(path: &Path, max: u64) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    let opened = {
        let mut options = OpenOptions::new();
        options.read(true);
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
        options.open(path)
    };
    // 재분석 지점이 아니고 소유자·DACL 이 소유자 전용인지 열린 핸들로 본다 — 아래 Unix mode·uid 검사의 대응.
    #[cfg(windows)]
    let opened = kasa_socket::private_store::open_existing(path, false);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Error::Storage),
    };
    let metadata = file.metadata().map_err(|_| Error::Storage)?;
    if !metadata.is_file() || metadata.len() > max {
        return Err(Error::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::Storage);
        }
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Storage)?;
    if bytes.len() as u64 > max {
        return Err(Error::Storage);
    }
    Ok(Some(bytes))
}

#[cfg(windows)]
fn private_directory(directory: &Path) -> Result<()> {
    kasa_socket::private_store::verify_dir(directory).map(drop).map_err(|_| Error::Storage)
}

#[cfg(not(windows))]
fn private_directory(directory: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(directory).map_err(|_| Error::Storage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::Storage);
        }
    }
    Ok(())
}

fn create_private(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    let created = {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        options.open(path)
    };
    #[cfg(windows)]
    let created = kasa_socket::private_store::create_new(path);
    let mut file = created.map_err(|_| Error::Storage)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Storage)
}

impl Vault {
    pub(crate) fn open(directory: PathBuf, domain: &'static str) -> Result<Self> {
        if !directory.is_absolute() || directory.file_name().is_none() {
            return Err(Error::Storage);
        }
        if !directory.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&directory).map_err(|_| Error::Storage)?;
            }
            #[cfg(windows)]
            kasa_socket::private_store::create_dir(&directory).map_err(|_| Error::Storage)?;
        }
        #[cfg(windows)]
        let directory_guard = kasa_socket::private_store::verify_dir(&directory).map_err(|_| Error::Storage)?;
        private_directory(&directory)?;
        let master = directory.join("master.key");
        let mut bytes = match read_private(&master, 32)? {
            Some(bytes) => bytes,
            None => {
                if std::fs::read_dir(&directory)
                    .map_err(|_| Error::Storage)?
                    .next()
                    .is_some()
                {
                    return Err(Error::Storage);
                }
                let key = random::<32>()?;
                create_private(&master, &key)?;
                #[cfg(unix)]
                File::open(&directory)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| Error::Storage)?;
                key.to_vec()
            }
        };
        let unbound =
            aead::UnboundKey::new(&aead::AES_256_GCM, &bytes).map_err(|_| Error::Storage)?;
        bytes.fill(0);
        let lock_path = directory.join("store.lock");
        if read_private(&lock_path, 0)?.is_none() {
            create_private(&lock_path, &[])?;
        }
        #[cfg(unix)]
        let lock = {
            let mut options = OpenOptions::new();
            options.read(true);
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
            let lock = options.open(&lock_path).map_err(|_| Error::Storage)?;
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(Error::Storage);
            }
            lock
        };
        // LockFileEx 는 핸들마다라 같은 프로세스의 두 번째 열기도 막힌다(flock 의 단일 쓰기 계약과 같다).
        #[cfg(windows)]
        let lock = {
            let lock = kasa_socket::private_store::open_existing(&lock_path, false).map_err(|_| Error::Storage)?;
            lock.try_lock().map_err(|_| Error::Storage)?;
            lock
        };
        Ok(Self {
            directory,
            domain,
            key: aead::LessSafeKey::new(unbound),
            _lock: lock,
            #[cfg(windows)]
            _directory: directory_guard,
        })
    }

    fn path(&self, account: &str) -> Result<PathBuf> {
        if !crate::relay_auth::valid_account_name(account) {
            return Err(Error::Invalid);
        }
        private_directory(&self.directory)?;
        Ok(self.directory.join(format!("{account}.sealed")))
    }

    /// Accounts that have a sealed file in this store.
    pub(crate) fn accounts(&self) -> Vec<String> {
        std::fs::read_dir(&self.directory)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| Some(entry.file_name().to_str()?.strip_suffix(".sealed")?.to_string()))
            .filter(|account| crate::relay_auth::valid_account_name(account))
            .collect()
    }

    pub(crate) fn read<T: DeserializeOwned>(&self, account: &str) -> Result<Option<T>> {
        let Some(bytes) = read_private(&self.path(account)?, MAX_ENVELOPE)? else {
            return Ok(None);
        };
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| Error::Storage)?;
        if envelope.version != 1 {
            return Err(Error::Storage);
        }
        let decode = |text| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .map_err(|_| Error::Storage)
        };
        let nonce: [u8; 12] = decode(&envelope.nonce)?
            .try_into()
            .map_err(|_| Error::Storage)?;
        let mut ciphertext = decode(&envelope.ciphertext)?;
        let aad = format!("{}:{account}", self.domain);
        let clear = self
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad.as_bytes()),
                &mut ciphertext,
            )
            .map_err(|_| Error::Storage)?;
        let value = serde_json::from_slice(clear).map_err(|_| Error::Storage);
        ciphertext.fill(0);
        value.map(Some)
    }

    pub(crate) fn write<T: Serialize>(&self, account: &str, value: &T) -> Result<()> {
        let path = self.path(account)?;
        if std::fs::symlink_metadata(&path)
            .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
        {
            return Err(Error::Storage);
        }
        // is_symlink 는 이름 대리 재분석 지점만 잡는다 — 정션·다른 태그·넓은 DACL 의 기존 파일도 덮지 않는다.
        #[cfg(windows)]
        if std::fs::symlink_metadata(&path).is_ok() {
            read_private(&path, MAX_ENVELOPE)?;
        }
        let nonce = random::<12>()?;
        let aad = format!("{}:{account}", self.domain);
        let mut clear = serde_json::to_vec(value).map_err(|_| Error::Storage)?;
        if clear.len() > 5 * 1024 * 1024 {
            return Err(Error::Storage);
        }
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad.as_bytes()),
                &mut clear,
            )
            .map_err(|_| Error::Storage)?;
        let envelope = Envelope {
            version: 1,
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
            ciphertext: base64::engine::general_purpose::STANDARD.encode(clear),
        };
        let bytes = serde_json::to_vec(&envelope).map_err(|_| Error::Storage)?;
        let temp = self
            .directory
            .join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
        let result = (|| {
            create_private(&temp, &bytes)?;
            std::fs::rename(&temp, &path).map_err(|_| Error::Storage)?;
            #[cfg(unix)]
            File::open(&self.directory)
                .and_then(|file| file.sync_all())
                .map_err(|_| Error::Storage)?;
            // Windows 엔 디렉터리 fsync 가 없다(NTFS 가 이름 바꾸기를 메타데이터 로그에 남긴다). 바뀐 자리의 파일이
            // tmp 의 소유자 전용 모양 그대로인지 다시 본다.
            #[cfg(windows)]
            read_private(&path, MAX_ENVELOPE)?.ok_or(Error::Storage)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

/// Windows 소유자 전용 보관함 — 만든 것의 모양, 단일 쓰기 잠금, 넓거나 링크인 폴더 거부, 저장 실패 때 원본 보존.
/// 넓힌 ACL·NULL DACL·다른 소유자 거부는 같은 검사를 쓰는 `kasa_socket::private_store` 시험이 본다.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use kasa_socket::private_store::{describe, Shape};

    const PRIVATE_DIR: Shape = Shape {
        reparse: false, directory: true, owner_is_user: true, dacl_present: true, dacl_protected: true, user_aces: 1, other_aces: 0,
    };
    const PRIVATE_FILE: Shape = Shape { directory: false, ..PRIVATE_DIR };

    fn scratch(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("kasa-vault-{name}-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".tmp")).collect()
    }

    #[test]
    fn vault_files_are_owner_only_and_one_writer_holds_the_store() {
        let dir = scratch("shape").join("store");
        let vault = Vault::open(dir.clone(), "kasa.test.v1").unwrap();
        vault.write("alice", &"first".to_string()).unwrap();
        vault.write("alice", &"second".to_string()).unwrap();
        assert_eq!(vault.read::<String>("alice").unwrap().as_deref(), Some("second"));
        assert_eq!(describe(&dir).unwrap(), PRIVATE_DIR);
        for name in ["master.key", "store.lock", "alice.sealed"] {
            assert_eq!(describe(&dir.join(name)).unwrap(), PRIVATE_FILE, "{name}");
        }
        assert!(leftovers(&dir).is_empty());
        assert!(Vault::open(dir.clone(), "kasa.test.v1").is_err(), "같은 보관함의 두 번째 열기");
        assert!(std::fs::rename(&dir, dir.with_file_name("moved")).is_err(), "열린 동안 폴더를 치울 수 없다");
        drop(vault);
        let again = Vault::open(dir.clone(), "kasa.test.v1").unwrap();
        assert_eq!(again.read::<String>("alice").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn inherited_or_linked_directories_are_refused() {
        let base = scratch("refuse");
        let inherited = base.join("inherited");
        std::fs::create_dir(&inherited).unwrap();
        assert!(Vault::open(inherited, "kasa.test.v1").is_err(), "상속 ACL 그대로인 폴더는 바로잡지 않고 거부");

        let real = base.join("real");
        drop(Vault::open(real.clone(), "kasa.test.v1").unwrap());
        let status = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(base.join("link"))
            .arg(&real)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(Vault::open(base.join("link"), "kasa.test.v1").is_err(), "정션 폴더는 따라가지 않는다");
        assert!(Vault::open(PathBuf::from("relative-store"), "kasa.test.v1").is_err());
    }

    /// 지우기 공유만 뺀 채 쥐면 사전 검사는 지나 tmp 를 만든 뒤 교체에서 막히고, 공유 0 이면 사전 검사에서 막힌다.
    /// 어느 쪽이든 값은 그대로, tmp 는 남지 않는다.
    #[test]
    fn a_failed_save_keeps_the_previous_value_and_no_tmp() {
        use std::os::windows::fs::OpenOptionsExt;
        const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
        let dir = scratch("busy").join("store");
        let vault = Vault::open(dir.clone(), "kasa.test.v1").unwrap();
        vault.write("alice", &"kept".to_string()).unwrap();
        for share in [SHARE_READ_WRITE, 0] {
            let held = std::fs::OpenOptions::new().read(true).share_mode(share).open(dir.join("alice.sealed")).unwrap();
            assert_eq!(vault.write("alice", &"lost".to_string()), Err(Error::Storage), "share {share}");
            drop(held);
            assert_eq!(vault.read::<String>("alice").unwrap().as_deref(), Some("kept"), "share {share}");
            assert!(leftovers(&dir).is_empty(), "share {share}");
        }
    }
}
