//! Per-account sealed files on the gateway: AES-256-GCM under a master key that lives in the same
//! private directory, with the domain and account name bound as associated data. Each store keeps
//! its own directory and key, so one leaked key does not open another store.

use base64::Engine as _;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fs::{File, OpenOptions};
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
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = match options.open(path) {
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
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| Error::Storage)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| Error::Storage)
}

impl Vault {
    pub(crate) fn open(directory: PathBuf, domain: &'static str) -> Result<Self> {
        // Unix permissions are a prerequisite until a tested Windows ACL implementation exists.
        if !cfg!(unix) || !directory.is_absolute() || directory.file_name().is_none() {
            return Err(Error::Storage);
        }
        if !directory.exists() {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&directory).map_err(|_| Error::Storage)?;
        }
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
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(&lock_path).map_err(|_| Error::Storage)?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(Error::Storage);
            }
        }
        Ok(Self {
            directory,
            domain,
            key: aead::LessSafeKey::new(unbound),
            _lock: lock,
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
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}
