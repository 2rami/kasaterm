use std::fmt;
#[cfg(unix)]
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
#[cfg(any(target_os = "macos", all(test, unix)))]
use std::time::{Duration, Instant};

use serde_json::Value;

type Result<T> = std::result::Result<T, &'static str>;
const LIMIT: usize = 64 * 1024;
#[cfg(all(target_os = "macos", not(test)))]
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Eq)]
struct Secret(Arc<[u8]>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    File,
    #[cfg(all(target_os = "macos", not(test)))]
    Keychain,
}

#[derive(Clone, PartialEq, Eq)]
enum Stored {
    Absent,
    Present(Backend, Secret),
}

#[derive(Clone)]
pub(crate) struct Snapshot {
    target: PathBuf,
    original: Stored,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SlotSnapshot([redacted])")
    }
}

#[derive(Clone)]
pub(crate) struct Staged {
    source: PathBuf,
    original: Stored,
}

impl fmt::Debug for Staged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StagedCredentials([redacted])")
    }
}

impl Staged {
    pub(crate) fn identity(&self) -> Option<kasa_mcp::VerifiedClaudeIdentity> {
        match &self.original {
            Stored::Present(_, blob) => super::verified_blob_identity(&blob.0),
            Stored::Absent => None,
        }
    }
}

pub(crate) struct Stage {
    root: PathBuf,
    storage: PathBuf,
    #[cfg(unix)]
    root_id: (u64, u64),
}

impl fmt::Debug for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PhoneLoginStage([redacted])")
    }
}

impl Stage {
    pub(crate) fn create() -> Result<Self> {
        for _ in 0..8 {
            let root = std::env::temp_dir().join(format!(
                "kasaterm-claude-phone-{}",
                uuid::Uuid::new_v4().simple()
            ));
            create_private_dir(&root)?;
            let storage = root.join("storage");
            let setup = (|| {
                create_private_dir(&storage)?;
                create_private_dir(&root.join("home"))?;
                #[cfg(all(target_os = "macos", not(test)))]
                if keychain_read(Some(&storage))?.is_some() {
                    return Err("staging_service_collision");
                }
                Ok(())
            })();
            if let Err(error) = setup {
                let _ = std::fs::remove_dir_all(&root);
                if error == "staging_service_collision" {
                    continue;
                }
                return Err(error);
            }
            #[cfg(unix)]
            let root_id = {
                use std::os::unix::fs::MetadataExt;
                let metadata =
                    std::fs::symlink_metadata(&root).map_err(|_| "staging_unavailable")?;
                (metadata.dev(), metadata.ino())
            };
            return Ok(Self {
                root,
                storage,
                #[cfg(unix)]
                root_id,
            });
        }
        Err("staging_service_collision")
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn storage_dir(&self) -> &Path {
        &self.storage
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        // The caller owns the CLI child and must reap it before dropping its isolated store.
        let Ok(metadata) = std::fs::symlink_metadata(&self.root) else {
            return;
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (metadata.dev(), metadata.ino()) != self.root_id {
                return;
            }
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            if let Ok((service, user)) = keychain_target(Some(&self.storage)) {
                // The generated service was absent before this stage was handed to the CLI.
                let _ = security(
                    &["delete-generic-password", "-s", &service, "-a", &user],
                    None,
                );
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn snapshot(target: &Path) -> Result<Snapshot> {
    valid_path(target)?;
    let _mutation = super::credential_mutation();
    Ok(Snapshot {
        target: target.into(),
        original: read_store(Some(target))?,
    })
}

pub(crate) fn read_staged(stage: &Stage) -> Result<Staged> {
    let original = read_store(Some(stage.storage_dir()))?;
    let Stored::Present(_, blob) = &original else {
        return Err("staging_credentials_absent");
    };
    oauth(&blob.0)?;
    Ok(Staged {
        source: stage.storage.clone(),
        original,
    })
}

pub(crate) fn promote(target: &Path, snapshot: &Snapshot, staged: &Staged) -> Result<()> {
    valid_path(target)?;
    if target != snapshot.target {
        return Err("credential_target_changed");
    }
    let _mutation = super::credential_mutation();
    promote_with(target, snapshot, staged, |blob| {
        super::verified_blob_identity(blob).is_some()
    })
}

fn promote_with(
    target: &Path,
    snapshot: &Snapshot,
    staged: &Staged,
    verified: impl Fn(&[u8]) -> bool,
) -> Result<()> {
    let Stored::Present(_, fresh) = &staged.original else {
        return Err("staging_credentials_absent");
    };
    if !verified(&fresh.0) {
        return Err("credential_identity_unverified");
    }
    if read_store(Some(&staged.source))? != staged.original {
        return Err("staging_credentials_changed");
    }
    if read_store(Some(target))? != snapshot.original {
        return Err("credential_target_changed");
    }
    let merged = merge_oauth(&snapshot.original, &fresh.0)?;
    if !verified(&fresh.0) {
        return Err("credential_identity_unverified");
    }
    write_store(Some(target), &snapshot.original, &merged)?;
    super::bump_workbench_generation();
    Ok(())
}

pub(crate) fn refresh_active(account_id: &str, staged: &Staged) -> Result<bool> {
    if account_id.is_empty() {
        return Ok(false);
    }
    let _mutation = super::credential_mutation();
    let Some(stamp_home) = super::active_dir() else {
        return Ok(false);
    };
    refresh_active_in(
        account_id,
        staged,
        &stamp_home,
        super::workbench_store().as_deref(),
        super::verified_blob_identity,
    )
}

fn refresh_active_in(
    account_id: &str,
    staged: &Staged,
    stamp_home: &Path,
    store: Option<&Path>,
    identity_of: impl Fn(&[u8]) -> Option<kasa_mcp::VerifiedClaudeIdentity>,
) -> Result<bool> {
    let Some((owner, digest)) = super::read_stamp_in(stamp_home) else {
        return Ok(false);
    };
    if owner != account_id {
        return Ok(false);
    }
    let Stored::Present(_, fresh) = &staged.original else {
        return Err("staging_credentials_absent");
    };
    let Some(fresh_identity) = identity_of(&fresh.0) else {
        return Err("credential_identity_unverified");
    };
    if read_store(Some(&staged.source))? != staged.original {
        return Err("staging_credentials_changed");
    }
    let current = read_store(store)?;
    let Stored::Present(_, old) = &current else {
        return Ok(false);
    };
    let exact_stamp = super::digest_of(&old.0) == digest;
    let same_identity = identity_of(&old.0).is_some_and(|identity| {
        identity.account_uuid == fresh_identity.account_uuid
            && identity.organization_uuid == fresh_identity.organization_uuid
    });
    if !exact_stamp && !same_identity {
        return Ok(false);
    }
    if same_identity && super::is_newer(&fresh.0, &old.0) {
        return Ok(false);
    }
    let merged = merge_oauth(&current, &fresh.0)?;
    if super::read_stamp_in(stamp_home).as_ref() != Some(&(owner, digest)) {
        return Ok(false);
    }
    if !identity_of(&fresh.0).is_some_and(|identity| {
        identity.account_uuid == fresh_identity.account_uuid
            && identity.organization_uuid == fresh_identity.organization_uuid
    }) {
        return Err("credential_identity_unverified");
    }
    write_store(store, &current, &merged)?;
    super::bump_workbench_generation();
    let identity = super::IdentityKey::from_verified(fresh_identity);
    super::write_stamp_with_identity_in(
        stamp_home,
        account_id,
        &super::digest_of(&merged),
        Some(&identity),
    );
    if super::read_stamp_in(stamp_home) != Some((account_id.to_string(), super::digest_of(&merged)))
    {
        return Err("credential_ownership_unconfirmed");
    }
    Ok(true)
}

fn oauth(blob: &[u8]) -> Result<serde_json::Map<String, Value>> {
    let value: Value = serde_json::from_slice(blob).map_err(|_| "credential_document_invalid")?;
    let oauth = value
        .get("claudeAiOauth")
        .and_then(Value::as_object)
        .ok_or("credential_document_invalid")?;
    if !oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
    {
        return Err("credential_document_invalid");
    }
    Ok(oauth.clone())
}

fn merge_oauth(original: &Stored, fresh: &[u8]) -> Result<Vec<u8>> {
    let mut document = match original {
        Stored::Absent => serde_json::Map::new(),
        Stored::Present(_, blob) => serde_json::from_slice::<Value>(&blob.0)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .ok_or("credential_document_invalid")?,
    };
    document.insert("claudeAiOauth".into(), Value::Object(oauth(fresh)?));
    let bytes = serde_json::to_vec(&document).map_err(|_| "credential_document_invalid")?;
    if bytes.len() > LIMIT {
        return Err("credential_document_too_large");
    }
    Ok(bytes)
}

fn valid_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err("credential_path_invalid");
    }
    #[cfg(test)]
    if !path.starts_with(std::env::temp_dir()) {
        return Err("credential_test_path_refused");
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err("credential_directory_invalid");
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err("credential_directory_unavailable");
        }
        _ => {}
    }
    Ok(())
}

fn read_store(dir: Option<&Path>) -> Result<Stored> {
    if let Some(dir) = dir {
        valid_path(dir)?;
    }
    #[cfg(all(target_os = "macos", not(test)))]
    if let Some(blob) = keychain_read(dir)? {
        return Ok(Stored::Present(Backend::Keychain, Secret(blob.into())));
    }
    let Some(dir) = dir else {
        #[cfg(all(target_os = "macos", not(test)))]
        return Ok(Stored::Absent);
        #[cfg(not(all(target_os = "macos", not(test))))]
        return Err("credential_path_invalid");
    };
    valid_path(dir)?;
    match read_private(&dir.join(".credentials.json"))? {
        Some(blob) => Ok(Stored::Present(Backend::File, Secret(blob.into()))),
        None => Ok(Stored::Absent),
    }
}

fn write_store(dir: Option<&Path>, original: &Stored, bytes: &[u8]) -> Result<()> {
    if read_store(dir)? != *original {
        return Err("credential_target_changed");
    }
    #[cfg(all(target_os = "macos", not(test)))]
    if matches!(
        original,
        Stored::Absent | Stored::Present(Backend::Keychain, _)
    ) {
        keychain_write(dir, bytes, !matches!(original, Stored::Absent))?;
        let expected = Stored::Present(Backend::Keychain, Secret(bytes.into()));
        return if read_store(dir)? == expected {
            Ok(())
        } else {
            Err("credential_write_unconfirmed")
        };
    }
    let dir = dir.ok_or("credential_path_invalid")?;
    write_private(dir, ".credentials.json", bytes, || {
        if read_store(Some(dir))? == *original {
            Ok(())
        } else {
            Err("credential_target_changed")
        }
    })?;
    if read_store(Some(dir))? != Stored::Present(Backend::File, Secret(bytes.into())) {
        return Err("credential_write_unconfirmed");
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| "credential_directory_unavailable")?;
    }
    #[cfg(windows)]
    {
        kasa_socket::private_store::create_dir(path)
            .map_err(|_| "credential_directory_unavailable")?;
    }
    Ok(())
}

fn verify_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| "credential_directory_unavailable")?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o022 != 0
        {
            return Err("credential_directory_not_private");
        }
    }
    #[cfg(windows)]
    {
        kasa_socket::private_store::verify_dir(path)
            .map_err(|_| "credential_directory_not_private")?;
    }
    Ok(())
}

fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("credential_read_failed"),
        Ok(_) => {}
    }
    verify_directory(path.parent().ok_or("credential_path_invalid")?)?;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| "credential_read_failed")?;
        let metadata = file.metadata().map_err(|_| "credential_read_failed")?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err("credential_file_not_private");
        }
        file
    };
    #[cfg(windows)]
    let file = kasa_socket::private_store::open_existing(path, false)
        .map_err(|_| "credential_file_not_private")?;
    let mut bytes = Vec::new();
    file.take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "credential_read_failed")?;
    if bytes.len() > LIMIT {
        return Err("credential_document_too_large");
    }
    Ok(Some(bytes))
}

fn write_private(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    before_replace: impl FnOnce() -> Result<()>,
) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_private_dir(dir)?,
        Err(_) => return Err("credential_directory_unavailable"),
        Ok(_) => {}
    }
    verify_directory(dir)?;
    #[cfg(windows)]
    let _directory_guard = kasa_socket::private_store::verify_dir(dir)
        .map_err(|_| "credential_directory_not_private")?;
    let temporary = dir.join(format!(
        ".phone-credentials-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    struct RemoveTemporary(PathBuf);
    impl Drop for RemoveTemporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = RemoveTemporary(temporary.clone());
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| "credential_write_failed")?
    };
    #[cfg(windows)]
    let mut file = kasa_socket::private_store::create_new(&temporary)
        .map_err(|_| "credential_write_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "credential_write_failed")?;
    drop(file);
    before_replace()?;
    std::fs::rename(&temporary, dir.join(name)).map_err(|_| "credential_write_failed")?;
    #[cfg(unix)]
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|_| "credential_write_unconfirmed")?;
    Ok(())
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_target(dir: Option<&Path>) -> Result<(String, String)> {
    let user = unsafe {
        let mut record: libc::passwd = std::mem::zeroed();
        let mut buffer = vec![0u8; 16 * 1024];
        let mut found = std::ptr::null_mut();
        if libc::getpwuid_r(
            libc::geteuid(),
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        ) != 0
            || found.is_null()
            || record.pw_name.is_null()
        {
            return Err("keychain_owner_unavailable");
        }
        std::ffi::CStr::from_ptr(record.pw_name)
            .to_str()
            .map_err(|_| "keychain_owner_unavailable")?
            .to_string()
    };
    if user.is_empty()
        || !user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err("keychain_owner_unavailable");
    }
    let service = super::service_name(dir);
    if !service
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b" -".contains(&byte))
    {
        return Err("keychain_service_invalid");
    }
    Ok((service, user))
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_read(dir: Option<&Path>) -> Result<Option<Vec<u8>>> {
    let (service, user) = keychain_target(dir)?;
    let (status, mut bytes) = security(
        &["find-generic-password", "-a", &user, "-s", &service, "-w"],
        None,
    )?;
    if status.code() == Some(44) {
        return Ok(None);
    }
    if !status.success() {
        return Err("keychain_read_failed");
    }
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
    {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err("credential_document_invalid");
    }
    Ok(Some(bytes))
}

#[cfg(all(target_os = "macos", not(test)))]
fn keychain_write(dir: Option<&Path>, bytes: &[u8], update: bool) -> Result<()> {
    let (service, user) = keychain_target(dir)?;
    let line = keychain_line(&service, &user, bytes, update)?;
    let (status, _) = security(&["-i"], Some(line))?;
    if status.success() {
        Ok(())
    } else {
        Err("keychain_write_unconfirmed")
    }
}

#[cfg(any(target_os = "macos", test))]
fn keychain_line(service: &str, user: &str, bytes: &[u8], update: bool) -> Result<Vec<u8>> {
    if service.is_empty()
        || user.is_empty()
        || !service
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b" ._-".contains(&byte))
        || !user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err("keychain_target_invalid");
    }
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let line = format!(
        "add-generic-password {}-a \"{user}\" -s \"{service}\" -X \"{hex}\"\n",
        if update { "-U " } else { "" }
    );
    if line.len() > 4032 {
        return Err("keychain_document_too_large");
    }
    Ok(line.into_bytes())
}

#[cfg(all(target_os = "macos", not(test)))]
fn security(args: &[&str], input: Option<Vec<u8>>) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    let mut command = std::process::Command::new("/usr/bin/security");
    command.args(args);
    bounded_child(command, input, CHILD_TIMEOUT)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn bounded_child(
    mut command: std::process::Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    use std::process::Stdio;
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "credential_child_start_failed")?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("credential_child_read_failed");
    };
    let stdin = child.stdin.take();
    let (sent, received) = std::sync::mpsc::channel();
    let started = std::thread::Builder::new()
        .name("phone-credential-io".into())
        .spawn(move || {
            let result = (|| {
                if let Some(bytes) = input {
                    stdin
                        .ok_or("credential_child_write_failed")?
                        .write_all(&bytes)
                        .map_err(|_| "credential_child_write_failed")?;
                }
                let mut bytes = Vec::new();
                stdout
                    .take(LIMIT as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "credential_child_read_failed")?;
                if bytes.len() > LIMIT {
                    return Err("credential_child_output_too_large");
                }
                Ok(bytes)
            })();
            let _ = sent.send(result);
        });
    if started.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("credential_child_start_failed");
    }
    let deadline = Instant::now() + timeout;
    let mut output = None;
    loop {
        if let Ok(value) = received.try_recv() {
            output = Some(value);
        }
        match child.try_wait() {
            Ok(Some(status)) if output.is_some() => {
                return output.take().unwrap().map(|bytes| (status, bytes))
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("credential_child_wait_failed");
            }
            _ => {}
        }
        if output.as_ref().is_some_and(std::result::Result::is_err) {
            let _ = child.kill();
            let _ = child.wait();
            return output.take().unwrap().map(|_| unreachable!());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("credential_child_timeout");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(token: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "claudeAiOauth":{"accessToken":token,"refreshToken":"fixture-refresh","expiresAt":4102444800000u64}
        })).unwrap()
    }

    fn install(dir: &Path, bytes: &[u8]) {
        write_private(dir, ".credentials.json", bytes, || Ok(())).unwrap();
    }

    fn setup() -> (Stage, PathBuf, Snapshot, Staged) {
        let stage = Stage::create().unwrap();
        let target = stage.root().join("target");
        create_private_dir(&target).unwrap();
        let mut original: Value = serde_json::from_slice(&document("fixture-old")).unwrap();
        original["mcpOAuth"] = serde_json::json!({"server":"fixture-other-secret"});
        install(&target, &serde_json::to_vec(&original).unwrap());
        let snapshot = snapshot(&target).unwrap();
        install(stage.storage_dir(), &document("fixture-new"));
        let staged = read_staged(&stage).unwrap();
        (stage, target, snapshot, staged)
    }

    #[test]
    fn promotion_preserves_other_credentials_and_does_not_touch_the_workbench() {
        let (_stage, target, snapshot, staged) = setup();
        promote_with(&target, &snapshot, &staged, |_| true).unwrap();
        let bytes = read_private(&target.join(".credentials.json"))
            .unwrap()
            .unwrap();
        let doc: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(doc["mcpOAuth"]["server"], "fixture-other-secret");
        assert_eq!(doc["claudeAiOauth"]["accessToken"], "fixture-new");
        assert!(snapshot.original != read_store(Some(&target)).unwrap());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    }

    #[test]
    fn failed_identity_or_changed_target_never_replaces_the_original() {
        let (_stage, target, snapshot, staged) = setup();
        assert_eq!(
            promote_with(&target, &snapshot, &staged, |_| false),
            Err("credential_identity_unverified")
        );
        assert!(snapshot.original == read_store(Some(&target)).unwrap());
        install(&target, &document("fixture-concurrent-refresh"));
        let concurrent = read_store(Some(&target)).unwrap();
        assert_eq!(
            promote_with(&target, &snapshot, &staged, |_| true),
            Err("credential_target_changed")
        );
        assert!(concurrent == read_store(Some(&target)).unwrap());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 1);
    }

    #[test]
    fn staging_changes_and_late_target_changes_are_detected() {
        let (stage, target, snapshot, staged) = setup();
        install(stage.storage_dir(), &document("fixture-other-account"));
        assert_eq!(
            promote_with(&target, &snapshot, &staged, |_| true),
            Err("staging_credentials_changed")
        );
        assert!(snapshot.original == read_store(Some(&target)).unwrap());
        let staged = read_staged(&stage).unwrap();
        let calls = std::cell::Cell::new(0);
        let verify = |_: &[u8]| {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                install(&target, &document("fixture-late-refresh"));
            }
            true
        };
        assert_eq!(
            promote_with(&target, &snapshot, &staged, verify),
            Err("credential_target_changed")
        );
        let current: Value = serde_json::from_slice(
            &read_private(&target.join(".credentials.json"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            current["claudeAiOauth"]["accessToken"],
            "fixture-late-refresh"
        );
    }

    #[test]
    fn absent_and_unreadable_storage_are_not_the_same_snapshot() {
        let stage = Stage::create().unwrap();
        let target = stage.root().join("target");
        create_private_dir(&target).unwrap();
        let empty = snapshot(&target).unwrap();
        assert!(empty.original == Stored::Absent);
        std::fs::create_dir(target.join(".credentials.json")).unwrap();
        assert!(snapshot(&target).is_err());
    }

    #[test]
    fn a_new_file_is_created_privately_and_debug_never_prints_credentials() {
        let stage = Stage::create().unwrap();
        let target = stage.root().join("target");
        let snapshot = snapshot(&target).unwrap();
        install(stage.storage_dir(), &document("fixture-new"));
        let staged = read_staged(&stage).unwrap();
        assert_eq!(format!("{snapshot:?}"), "SlotSnapshot([redacted])");
        assert_eq!(format!("{staged:?}"), "StagedCredentials([redacted])");
        assert_eq!(format!("{stage:?}"), "PhoneLoginStage([redacted])");
        assert!(
            staged.identity().is_none(),
            "a fixture token has no provider proof"
        );
        promote_with(&target, &snapshot, &staged, |_| true).unwrap();
        verify_directory(&target).unwrap();
        assert!(read_private(&target.join(".credentials.json"))
            .unwrap()
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_modes_and_symlinks_are_refused_without_modification() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (stage, target, snapshot, staged) = setup();
        let path = target.join(".credentials.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(snapshot.original != Stored::Absent);
        assert!(read_private(&path).is_err());
        assert!(promote_with(&target, &snapshot, &staged, |_| true).is_err());
        let bytes = std::fs::read(&path).unwrap();
        let link_target = stage.root().join("linked");
        create_private_dir(&link_target).unwrap();
        symlink(&path, link_target.join(".credentials.json")).unwrap();
        assert!(super::snapshot(&link_target).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let linked_directory = stage.root().join("linked-directory");
        symlink(&target, &linked_directory).unwrap();
        assert_eq!(
            valid_path(&linked_directory),
            Err("credential_directory_invalid")
        );
        assert!(super::snapshot(&linked_directory).is_err());
        assert!(read_store(Some(&linked_directory)).is_err());
        let ordinary_file = stage.root().join("not-a-directory");
        std::fs::write(&ordinary_file, b"fixture").unwrap();
        assert_eq!(
            valid_path(&ordinary_file),
            Err("credential_directory_invalid")
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_readable_directory_keeps_private_files_protected() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (stage, target, original, staged) = setup();
        assert_eq!(
            std::fs::metadata(stage.root()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(stage.storage_dir()).unwrap().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(snapshot(&target).is_ok());
        promote_with(&target, &original, &staged, |_| true).unwrap();
        assert_eq!(
            std::fs::metadata(target.join(".credentials.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
        for mode in [0o775, 0o757] {
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(snapshot(&target).is_err());
        }
    }

    #[test]
    fn a_failed_precommit_check_removes_only_the_new_temporary_file() {
        let (_stage, target, original, _) = setup();
        assert_eq!(
            write_private(
                &target,
                ".credentials.json",
                &document("fixture-unused"),
                || Err("cancelled")
            ),
            Err("cancelled")
        );
        assert!(read_store(Some(&target)).unwrap() == original.original);
        assert_eq!(std::fs::read_dir(target).unwrap().count(), 1);
    }

    fn fixture_identity(_: &[u8]) -> Option<kasa_mcp::VerifiedClaudeIdentity> {
        Some(kasa_mcp::VerifiedClaudeIdentity {
            account_uuid: uuid::Uuid::from_u128(1).to_string(),
            organization_uuid: uuid::Uuid::from_u128(2).to_string(),
            email: "fixture@example.test".into(),
            org: "Fixture".into(),
        })
    }

    #[test]
    fn runtime_refresh_preserves_other_keys_and_records_new_ownership() {
        let (stage, _, _, staged) = setup();
        let bench = stage.root().join("bench");
        let stamp = stage.root().join("stamp");
        create_private_dir(&bench).unwrap();
        create_private_dir(&stamp).unwrap();
        let mut old: Value = serde_json::from_slice(&document("fixture-old")).unwrap();
        old["mcpOAuth"] = serde_json::json!({"keep":true});
        old["claudeAiOauth"]["expiresAt"] = 1.into();
        let old = serde_json::to_vec(&old).unwrap();
        install(&bench, &old);
        super::super::write_stamp_in(&stamp, "slot", &super::super::digest_of(&old));
        let identity =
            |bytes: &[u8]| (bytes != old.as_slice()).then(|| fixture_identity(bytes).unwrap());
        assert_eq!(
            refresh_active_in("slot", &staged, &stamp, Some(&bench), identity),
            Ok(true)
        );
        let bytes = read_private(&bench.join(".credentials.json"))
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["mcpOAuth"]["keep"], true);
        assert_eq!(value["claudeAiOauth"]["accessToken"], "fixture-new");
        assert_eq!(
            super::super::read_stamp_in(&stamp),
            Some(("slot".into(), super::super::digest_of(&bytes)))
        );
    }

    #[test]
    fn runtime_refresh_cannot_overwrite_an_unverified_external_login_or_newer_token() {
        let (stage, _, _, staged) = setup();
        let bench = stage.root().join("bench");
        let stamp = stage.root().join("stamp");
        create_private_dir(&bench).unwrap();
        create_private_dir(&stamp).unwrap();
        let old = document("fixture-external");
        install(&bench, &old);
        super::super::write_stamp_in(&stamp, "slot", "different-original");
        let identity =
            |bytes: &[u8]| (bytes != old.as_slice()).then(|| fixture_identity(bytes).unwrap());
        assert_eq!(
            refresh_active_in("slot", &staged, &stamp, Some(&bench), identity),
            Ok(false)
        );
        assert_eq!(
            read_private(&bench.join(".credentials.json"))
                .unwrap()
                .unwrap(),
            old
        );
        let mut newer: Value = serde_json::from_slice(&old).unwrap();
        newer["claudeAiOauth"]["expiresAt"] = 4102444809000u64.into();
        let newer = serde_json::to_vec(&newer).unwrap();
        install(&bench, &newer);
        assert_eq!(
            refresh_active_in("slot", &staged, &stamp, Some(&bench), fixture_identity),
            Ok(false)
        );
        assert_eq!(
            read_private(&bench.join(".credentials.json"))
                .unwrap()
                .unwrap(),
            newer
        );
        assert_eq!(
            refresh_active_in(
                "different-slot",
                &staged,
                &stamp,
                Some(&bench),
                fixture_identity
            ),
            Ok(false)
        );
    }

    #[test]
    fn late_runtime_updates_cannot_be_replaced_with_the_phone_login() {
        let (stage, _, _, staged) = setup();
        let bench = stage.root().join("bench");
        let stamp = stage.root().join("stamp");
        create_private_dir(&bench).unwrap();
        create_private_dir(&stamp).unwrap();
        let old = document("fixture-original");
        install(&bench, &old);
        super::super::write_stamp_in(&stamp, "slot", &super::super::digest_of(&old));
        let calls = std::cell::Cell::new(0);
        let identity = |bytes: &[u8]| {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                install(&bench, &document("fixture-concurrent"));
            }
            fixture_identity(bytes)
        };
        assert_eq!(
            refresh_active_in("slot", &staged, &stamp, Some(&bench), identity),
            Err("credential_target_changed")
        );
        let value: Value = serde_json::from_slice(
            &read_private(&bench.join(".credentials.json"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["claudeAiOauth"]["accessToken"], "fixture-concurrent");
        assert_eq!(
            super::super::read_stamp_in(&stamp),
            Some(("slot".into(), super::super::digest_of(&old)))
        );
    }

    #[test]
    fn staging_cleanup_has_one_unique_root_and_preserves_its_neighbours() {
        let first = Stage::create().unwrap();
        let second = Stage::create().unwrap();
        let removed = first.root().to_path_buf();
        assert_ne!(first.root(), second.root());
        drop(first);
        assert!(!removed.exists());
        assert!(second.root().is_dir());
    }

    #[test]
    fn interactive_keychain_input_is_bounded_and_never_interpolates_raw_secret() {
        let body = b"fixture-token with \"quotes\"\n";
        let line = keychain_line(
            "Claude Code-credentials-fixture",
            "fixture-user",
            body,
            true,
        )
        .unwrap();
        let text = String::from_utf8(line).unwrap();
        assert!(text.starts_with("add-generic-password -U "));
        assert!(!text.contains("fixture-token"));
        assert_eq!(text.lines().count(), 1);
        assert!(keychain_line("service", "bad\"\nowner", body, true).is_err());
        assert!(keychain_line("service", "owner", &vec![b'x'; 3000], true).is_err());
        assert!(
            !String::from_utf8(keychain_line("service", "owner", body, false).unwrap())
                .unwrap()
                .contains(" -U ")
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_children_use_stdin_and_are_reaped_on_timeout() {
        let mut echo = std::process::Command::new("/bin/sh");
        echo.args(["-c", "IFS= read -r line; printf '%s' \"$line\""]);
        let (status, bytes) = bounded_child(
            echo,
            Some(b"fixture-input\n".to_vec()),
            Duration::from_secs(2),
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(bytes, b"fixture-input");
        let mut blocked = std::process::Command::new("/bin/sh");
        blocked.args(["-c", "exec sleep 10"]);
        let before = Instant::now();
        assert!(matches!(
            bounded_child(blocked, None, Duration::from_millis(30)),
            Err("credential_child_timeout")
        ));
        assert!(before.elapsed() < Duration::from_secs(2));
    }
}
