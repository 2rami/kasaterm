//! Durable, receiver-owned idempotency and fail-closed message transitions.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::{Path, PathBuf}};

pub const MAX_BODY: usize = 16 * 1024;
const MAX_RECORDS: usize = 1024;
const MAX_STORAGE: usize = 4 * 1024 * 1024;
pub const RECEIPT_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Address {
    pub machine_id: String,
    pub surface_key: String,
    pub surface_id: String,
    pub session_id: String,
    pub instance_id: String,
}
impl Address {
    pub fn parse(value: &Value) -> Result<Self> {
        let address: Self = serde_json::from_value(value.clone()).context("full current address required (machine, surface, session and instance)")?;
        for value in [&address.machine_id, &address.surface_key, &address.surface_id, &address.session_id, &address.instance_id] {
            ensure!(!value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control), "invalid address component");
        }
        Ok(address)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State { Accepted, Dispatching, Submitted, Failed, Uncertain }

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Record {
    pub message_id: String,
    pub address: Address,
    pub body: String,
    pub body_hash: String,
    pub state: State,
    pub reason: String,
    pub accepted_at_ms: u64,
    pub updated_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub reject_if_busy: bool,
    #[serde(default)]
    pub receiver_agent_pid: u32,
}
impl Record {
    pub fn receipt(&self) -> Value {
        json!({"message_id":self.message_id,"address":self.address,"body_hash":self.body_hash,
            "state":self.state,"reason":self.reason,"accepted_at_ms":self.accepted_at_ms,
            "updated_at_ms":self.updated_at_ms,"expires_at_ms":self.expires_at_ms,
            "receipt_expires_at_ms":issued_at(&self.message_id).unwrap_or(0).saturating_add(RECEIPT_LIFETIME_MS)})
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
pub fn normalize(body: &str) -> Result<String> {
    ensure!(body.len() <= MAX_BODY, "tell body exceeds 16 KiB");
    let body = body.replace("\r\n", "\n");
    ensure!(!body.trim().is_empty(), "tell body is empty");
    ensure!(!body.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "tell body contains terminal control characters");
    Ok(body)
}
pub fn fingerprint(body: &str) -> String {
    let mut h = 0xcbf29ce484222325u64;
    for b in body.bytes() { h = (h ^ b as u64).wrapping_mul(0x100000001b3); }
    format!("fnv1a64:{h:016x}")
}
pub fn new_message_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("kt1.{}.{:x}-{:x}-{:x}",now_ms(),std::process::id(),nonce,NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed))
}
pub fn issued_at(id: &str) -> Result<u64> {
    ensure!(id.len() <= 128,"invalid message_id");
    let parts: Vec<_> = id.split('.').collect();
    ensure!(parts.len() == 3 && parts[0] == "kt1" && parts[2].len() >= 16
        && parts[2].bytes().all(|b|b.is_ascii_hexdigit() || b == b'-'),"message_id must be kt1.<unix-ms>.<unique-hex-nonce>");
    parts[1].parse().context("invalid message issuance time")
}
pub fn valid_id(id: &str) -> Result<()> {
    let issued = issued_at(id)?;
    let now = now_ms();
    ensure!(issued <= now.saturating_add(120_000),"message ID is from the future");
    ensure!(issued.saturating_add(RECEIPT_LIFETIME_MS) > now,"receipt_expired: old message IDs cannot be submitted again");
    Ok(())
}

pub struct Ledger { path: PathBuf, records: BTreeMap<String, Record>, _lock: fs::File }
impl Ledger {
    pub fn open(path: PathBuf) -> Result<Self> {
        #[cfg(windows)] anyhow::bail!("private tell receipt storage requires Windows ACL enforcement");
        let directory = path.parent().context("tell storage parent unavailable")?;
        fs::create_dir_all(directory)?;
        ensure!(!fs::symlink_metadata(directory)?.file_type().is_symlink(),"tell directory cannot be a symlink");
        let mut options = fs::OpenOptions::new(); options.read(true).write(true).create(true);
        #[cfg(unix)] {
            use std::os::unix::fs::{MetadataExt,OpenOptionsExt,PermissionsExt};
            ensure!(fs::metadata(directory)?.uid() == unsafe { libc::geteuid() },"tell directory owner mismatch");
            fs::set_permissions(directory,fs::Permissions::from_mode(0o700))?;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let lock = options.open(directory.join("receiver.lock"))?;
        #[cfg(unix)] {
            use std::os::fd::AsRawFd;
            ensure!(unsafe { libc::flock(lock.as_raw_fd(),libc::LOCK_EX | libc::LOCK_NB) } == 0,"another receiver owns this tell storage");
        }
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            ensure!(metadata.is_file() && !metadata.file_type().is_symlink(),"tell ledger must be a regular file");
        }
        let records: BTreeMap<String, Record> = match fs::read(&path) {
            Ok(bytes) => { ensure!(bytes.len() <= MAX_STORAGE, "tell ledger exceeds storage limit"); serde_json::from_slice(&bytes).context("corrupt tell ledger; delivery stopped")? },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        ensure!(records.len() <= MAX_RECORDS,"tell ledger exceeds record limit");
        for (id,record) in &records {
            ensure!(id == &record.message_id && issued_at(id).is_ok() && normalize(&record.body)? == record.body
                && fingerprint(&record.body) == record.body_hash,"tell ledger record integrity mismatch");
        }
        let mut ledger = Self { path, records, _lock: lock };
        let mut recovered = false;
        for record in ledger.records.values_mut() {
            if record.state == State::Dispatching {
                record.state = State::Uncertain;
                record.reason = "receiver restarted during dispatch; automatic retry prohibited".into();
                record.updated_at_ms = now_ms();
                recovered = true;
            }
        }
        if recovered { ledger.persist()?; }
        Ok(ledger)
    }
    fn persist(&self) -> Result<()> {
        let bytes = serde_json::to_vec(&self.records)?;
        ensure!(bytes.len() <= MAX_STORAGE, "tell storage is full");
        private_write(&self.path, &bytes)
    }
    pub fn existing(&self, id: &str, address: &Address, body: &str) -> Result<Option<Record>> {
        valid_id(id)?;
        if let Some(record) = self.records.get(id) {
            ensure!(record.address == *address && record.body == body, "message_id already belongs to a different body or target");
            return Ok(Some(record.clone()));
        }
        Ok(None)
    }
    pub fn accept(&mut self, id: &str, address: Address, body: String, ttl_seconds: u64) -> Result<Record> {
        self.accept_with_policy(id,address,body,ttl_seconds,false,0)
    }
    pub fn accept_with_policy(&mut self, id: &str, address: Address, body: String, ttl_seconds: u64, reject_if_busy: bool, receiver_agent_pid: u32) -> Result<Record> {
        let body = normalize(&body)?;
        if let Some(record) = self.existing(id, &address, &body)? { return Ok(record); }
        self.prune_at(now_ms());
        ensure!(self.records.len() < MAX_RECORDS, "tell receipt storage is full");
        ensure!((1..=3600).contains(&ttl_seconds), "ttl_seconds must be between 1 and 3600");
        let now = now_ms();
        let record = Record { message_id: id.into(), address, body_hash: fingerprint(&body), body,
            state: State::Accepted, reason: "stored; waiting for safe empty input".into(), accepted_at_ms: now,
            updated_at_ms: now, expires_at_ms: (now + ttl_seconds * 1000).min(issued_at(id)? + RECEIPT_LIFETIME_MS), reject_if_busy, receiver_agent_pid };
        self.records.insert(id.into(), record.clone());
        if let Err(error) = self.persist() { self.records.remove(id); return Err(error); }
        Ok(record)
    }
    fn prune_at(&mut self, now: u64) {
        self.records.retain(|id,_|issued_at(id).is_ok_and(|issued|issued.saturating_add(RECEIPT_LIFETIME_MS) > now));
    }
    pub fn status(&self, id: &str, address: &Address) -> Result<Record> {
        valid_id(id)?;
        let record = self.records.get(id).context("message_id not found on this receiver")?;
        ensure!(&record.address == address, "receipt address mismatch");
        Ok(record.clone())
    }
    pub fn pending(&self) -> Vec<Record> {
        self.records.values().filter(|r|r.state == State::Accepted
            && !self.records.values().any(|other|other.address.surface_id == r.address.surface_id && other.state == State::Dispatching))
            .cloned().collect()
    }
    pub fn transition(&mut self, id: &str, state: State, reason: &str) -> Result<Record> {
        let old = self.records.get(id).context("unknown message_id")?.clone();
        let allowed = matches!((old.state, state), (State::Accepted, State::Dispatching | State::Failed) | (State::Dispatching, State::Submitted | State::Uncertain | State::Accepted | State::Failed));
        ensure!(allowed, "invalid tell state transition");
        let mut record = old.clone();
        record.state = state;
        record.reason = reason.into();
        record.updated_at_ms = now_ms();
        self.records.insert(id.into(), record.clone());
        if let Err(error) = self.persist() {
            // A dispatch whose final durable update failed must remain non-retriable.
            self.records.insert(id.into(), if old.state == State::Dispatching { Record {
                state: State::Uncertain, reason: format!("receipt persistence failed: {error}"), ..old
            }} else { old });
            return Err(error);
        }
        Ok(record)
    }

    pub fn expire_pending(&mut self, now: u64) -> Result<()> {
        let expired: Vec<_> = self.records.values().filter(|r|r.state == State::Accepted && r.expires_at_ms <= now).cloned().collect();
        if expired.is_empty() { return Ok(()); }
        for old in &expired {
            let record = self.records.get_mut(&old.message_id).unwrap();
            record.state = State::Failed;
            record.reason = "queued message expired".into();
            record.updated_at_ms = now;
        }
        if let Err(error) = self.persist() {
            for record in expired { self.records.insert(record.message_id.clone(),record); }
            return Err(error);
        }
        Ok(())
    }
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(windows)] anyhow::bail!("private tell receipt storage is unsupported until Windows ACL enforcement is available");
    let dir = path.parent().context("tell storage needs a parent directory")?;
    fs::create_dir_all(dir)?;
    ensure!(!fs::symlink_metadata(dir)?.file_type().is_symlink(), "tell storage directory must not be a symlink");
    #[cfg(unix)] {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(fs::metadata(dir)?.uid() == unsafe { libc::geteuid() }, "tell directory owner mismatch");
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dir.join(format!(".tell-{}-{}.tmp", std::process::id(), now_ms()));
    let mut options = fs::OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(&tmp)?;
    let result = (|| -> Result<()> { file.write_all(bytes)?; file.sync_all()?; fs::rename(&tmp, path)?;
        #[cfg(unix)] fs::File::open(dir)?.sync_all()?;
        Ok(()) })();
    if result.is_err() { let _ = fs::remove_file(&tmp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn address() -> Address { Address { machine_id:"machine".into(),surface_key:"key".into(),surface_id:"%1".into(),session_id:"session".into(),instance_id:"instance".into() } }
    fn ledger() -> Ledger {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        Ledger::open(std::env::temp_dir().join(format!("kasa-tell-{}-{}-{n}",std::process::id(),now_ms())).join("ledger.json")).unwrap()
    }
    #[test] fn control_injection_is_rejected_and_crlf_normalizes() {
        assert_eq!(normalize("hello\r\nworld\t!").unwrap(), "hello\nworld\t!");
        for body in ["hi\r", "\x15hi", "\x1b[201~", "\x00", "\u{85}"] { assert!(normalize(body).is_err()); }
        assert!(normalize(&"x".repeat(MAX_BODY+1)).is_err());
    }
    #[test] fn duplicate_id_does_not_reinject_or_retarget() {
        let mut ledger = ledger();
        let id = new_message_id();
        ledger.accept(&id,address(),"hello".into(),900).unwrap();
        assert_eq!(ledger.accept(&id,address(),"hello".into(),900).unwrap().state,State::Accepted);
        assert!(ledger.accept(&id,address(),"different".into(),900).is_err());
        let mut changed = address(); changed.session_id="new".into();
        assert!(ledger.accept(&id,changed,"hello".into(),900).is_err());
        assert_eq!(ledger.pending().len(),1);
    }
    #[test] fn restart_after_dispatch_is_uncertain_and_not_pending() {
        let mut ledger = ledger();
        let id = new_message_id();
        ledger.accept(&id,address(),"hello".into(),900).unwrap();
        ledger.transition(&id,State::Dispatching,"writing").unwrap();
        let path = ledger.path.clone();
        drop(ledger);
        let restored = Ledger::open(path).unwrap();
        assert_eq!(restored.status(&id,&address()).unwrap().state,State::Uncertain);
        assert!(restored.pending().is_empty());
    }
    #[test] fn submitted_does_not_claim_read_and_cannot_dispatch_again() {
        let mut ledger = ledger();
        let id = new_message_id();
        ledger.accept(&id,address(),"hello".into(),900).unwrap();
        ledger.transition(&id,State::Dispatching,"writing").unwrap();
        let receipt = ledger.transition(&id,State::Submitted,"writes succeeded").unwrap().receipt();
        assert!(receipt.get("read").is_none());
        assert!(ledger.transition(&id,State::Dispatching,"retry").is_err());
    }
    #[test] fn bounded_retention_never_reaccepts_a_forgotten_id() {
        let mut ledger = ledger();
        let old = format!("kt1.{}.0123456789abcdef",now_ms().saturating_sub(RECEIPT_LIFETIME_MS + 1));
        assert!(ledger.accept(&old,address(),"hello".into(),900).unwrap_err().to_string().contains("receipt_expired"));
        let id = new_message_id();
        ledger.accept(&id,address(),"hello".into(),900).unwrap();
        ledger.prune_at(issued_at(&id).unwrap() + RECEIPT_LIFETIME_MS);
        assert!(ledger.records.is_empty());
    }
    #[test] fn another_receiver_cannot_overwrite_the_same_ledger() {
        let ledger = ledger();
        assert!(Ledger::open(ledger.path.clone()).is_err());
    }
    #[test] fn one_dispatch_blocks_another_message_for_the_same_pane() {
        let mut ledger = ledger();
        let first = new_message_id();
        ledger.accept(&first,address(),"first".into(),900).unwrap();
        ledger.accept(&new_message_id(),address(),"second".into(),900).unwrap();
        ledger.transition(&first,State::Dispatching,"writing").unwrap();
        assert!(ledger.pending().is_empty());
        ledger.transition(&first,State::Uncertain,"connection lost").unwrap();
        assert_eq!(ledger.pending().len(),1);
    }
    #[test] fn expiration_covers_every_recipient_before_batch_selection() {
        let mut ledger = ledger();
        for index in 0..12 {
            let mut target = address(); target.surface_id = format!("%{index}");
            ledger.accept(&new_message_id(),target,"hello".into(),1).unwrap();
        }
        ledger.expire_pending(now_ms()+2000).unwrap();
        assert!(ledger.pending().is_empty());
        assert!(ledger.records.values().all(|record|record.state == State::Failed));
    }
    #[test] fn proven_zero_write_deferral_is_retryable_but_uncertain_is_not() {
        let mut ledger = ledger();
        let id = new_message_id();
        ledger.accept(&id,address(),"hello".into(),900).unwrap();
        ledger.transition(&id,State::Dispatching,"proof prepared").unwrap();
        ledger.transition(&id,State::Accepted,"no bytes written; approval appeared").unwrap();
        assert_eq!(ledger.pending().len(),1);
        ledger.transition(&id,State::Dispatching,"writing").unwrap();
        ledger.transition(&id,State::Uncertain,"connection lost").unwrap();
        assert!(ledger.transition(&id,State::Accepted,"retry").is_err());
    }
}
