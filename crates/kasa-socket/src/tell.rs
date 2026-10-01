//! Durable, receiver-owned idempotency and fail-closed message transitions.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io::Write, path::{Path, PathBuf}};

pub const MAX_BODY: usize = 16 * 1024;
pub const MAX_TITLE_CHARS: usize = 60;
const MAX_RECORDS: usize = 1024;
const MAX_STORAGE: usize = 4 * 1024 * 1024;
pub const RECEIPT_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
/// 줄 선 쪽지를 버리기까지(초). 상한(1시간)으로 둔다. 2026-10-01 이 맥북 영수증 하루치 61건 중 21건이 옛 기본
/// 15분에 만료됐는데, 막은 것은 사람이 Enter 를 칠 때까지 안 꺼지던 초안 표시였고(그 뒤로는 화면이 판정한다)
/// 늦게 들어간 것도 그 표시가 풀린 순간이었다. 남는 기다림은 사람의 진짜 초안·승인 화면뿐이라 사람이 자리를
/// 비운 만큼 길어지고, 만료된 21건 중 9건은 시간이 지나도 뜻이 안 바래는 완료 보고였다. 대신 기다리는 동안
/// 받는 창 입력칸 아래에 대기 표시가 뜨고, 보낸 창에는 2분 뒤 막힌 까닭과 버릴 시각이, 버리면 그 사실이 간다.
pub const QUEUE_TTL_SECONDS: u64 = 3600;

/// 전달을 미룬 까닭. 영수증 `reason` 의 `waiting:<낱말> — …` 로 실려 보낸 쪽 CLI·받는 쪽 화면이 무엇이
/// 막았는지 가른다 — 하나로 뭉친 문장으로는 「입력칸이 비었는데 왜」를 못 풀었다(2026-10-01).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold { Draft, Typing, Composition, Approval, Closed, PasteMode, Identity }

impl Hold {
    const ALL: [Hold; 7] = [Hold::Draft, Hold::Typing, Hold::Composition, Hold::Approval, Hold::Closed, Hold::PasteMode, Hold::Identity];
    pub fn word(self) -> &'static str {
        match self {
            Hold::Draft => "draft", Hold::Typing => "typing", Hold::Composition => "composition", Hold::Approval => "approval",
            Hold::Closed => "closed", Hold::PasteMode => "paste_mode", Hold::Identity => "identity",
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Hold::Draft => "waiting:draft — receiver input box has text; delivered once it is empty",
            Hold::Typing => "waiting:typing — receiver typed within the last seconds",
            Hold::Composition => "waiting:composition — receiver is composing with an IME",
            Hold::Approval => "waiting:approval — approval or question on the receiver screen",
            Hold::Closed => "waiting:closed — receiver input is closed",
            Hold::PasteMode => "waiting:paste_mode — receiver screen does not accept a paste now",
            Hold::Identity => "waiting:identity — identity proof went stale; retrying",
        }
    }
    /// 영수증 사유 → 까닭. 옛 판이 쓴 뭉친 문장은 `None`.
    pub fn from_reason(reason: &str) -> Option<Self> {
        let word = reason.strip_prefix("waiting:")?.split(' ').next()?;
        Self::ALL.into_iter().find(|hold| hold.word() == word)
    }
    /// 사람에게 하는 말 — 무엇이 막았나.
    pub fn cause(self) -> &'static str {
        match self {
            Hold::Draft => "받는 창 입력칸에 쓰던 글이 있어요",
            Hold::Typing => "받는 창에서 방금 키를 쳤어요",
            Hold::Composition => "받는 창에서 글자를 조합하는 중이에요",
            Hold::Approval => "받는 창이 승인·질문 화면이에요",
            Hold::Closed => "받는 창 입력이 닫혀 있어요",
            Hold::PasteMode => "받는 창 화면이 지금 붙여넣기를 안 받아요",
            Hold::Identity => "받는 창 확인이 늦어 다시 보는 중이에요",
        }
    }
    /// 사람에게 하는 말 — 언제 들어가나.
    pub fn remedy(self) -> &'static str {
        match self {
            Hold::Draft => "입력칸을 비우면 들어가요",
            Hold::Composition => "글자 조합이 끝나면 들어가요",
            Hold::Approval => "승인·질문에 답하면 들어가요",
            Hold::Typing | Hold::Identity => "곧 들어가요",
            Hold::Closed | Hold::PasteMode => "지금은 못 넣어요",
        }
    }
}

/// epoch ms → 이 기기 시각 `HH:MM`. 쪽지를 버릴 시각을 사람 말로 할 때 쓴다.
pub fn clock_hm(ms: u64) -> Option<String> {
    #[cfg(unix)]
    {
        let time = (ms / 1000).min(libc::time_t::MAX as u64) as libc::time_t;
        let mut local: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&time, &mut local) }.is_null() {
            return Some(format!("{:02}:{:02}", local.tm_hour, local.tm_min));
        }
    }
    let _ = ms;
    None
}

/// 옛 CLI 가 모르는 옵션을 본문 머리로 흘려보낸 흔적(`--title 제목 --stdin` 이 본문이 되고 진짜 본문은
/// 사라진 2026-10-01 실측). 받는 쪽에서 거절해야 보낸 쪽이 틀린 글이 갔다는 것을 그 자리에서 안다.
pub fn leaked_cli_flag(body: &str) -> Option<&'static str> {
    let mut text = body.trim_start();
    if let Some(rest) = text.strip_prefix('⟦').and_then(|rest| rest.split_once('⟧')) {
        text = rest.1.trim_start();
    }
    ["--title", "--stdin", "--id", "--address", "--force"].into_iter()
        .find(|flag| text.strip_prefix(flag).is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace)))
}

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
    /// 받는 학생의 「지금 일」. 비어 있지 않으면 전달이 끝난 순간 그 창 이름이 이것으로 바뀐다 —
    /// 세션 이름이 처음 붙인 제목으로 남아 일이 바뀌어도 안 따라오던 자리다(docs/boards.md 「지금 일」).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
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
/// 「지금 일」 한 줄 — 공백을 하나로 접고 `MAX_TITLE_CHARS` 자에서 자른다. 창 머리·사이드바 한 줄에 들어간다.
pub fn normalize_title(title: &str) -> Result<String> {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(!title.chars().any(char::is_control), "title contains control characters");
    Ok(title.chars().take(MAX_TITLE_CHARS).collect())
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
        self.accept_with_policy(id,address,body,ttl_seconds,false,0,String::new())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn accept_with_policy(&mut self, id: &str, address: Address, body: String, ttl_seconds: u64, reject_if_busy: bool, receiver_agent_pid: u32, title: String) -> Result<Record> {
        let body = normalize(&body)?;
        if let Some(record) = self.existing(id, &address, &body)? { return Ok(record); }
        self.prune_at(now_ms());
        ensure!(self.records.len() < MAX_RECORDS, "tell receipt storage is full");
        ensure!((1..=3600).contains(&ttl_seconds), "ttl_seconds must be between 1 and 3600");
        let now = now_ms();
        let record = Record { message_id: id.into(), address, body_hash: fingerprint(&body), body,
            state: State::Accepted, reason: "stored; waiting for safe empty input".into(), accepted_at_ms: now,
            updated_at_ms: now, expires_at_ms: (now + ttl_seconds * 1000).min(issued_at(id)? + RECEIPT_LIFETIME_MS), reject_if_busy, receiver_agent_pid, title: normalize_title(&title)? };
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
    #[test] fn title_is_one_short_line_and_survives_the_ledger() {
        assert_eq!(normalize_title("  작업현황 구현\n ④  지금 일 ").unwrap(), "작업현황 구현 ④ 지금 일");
        assert_eq!(normalize_title(&"가".repeat(MAX_TITLE_CHARS + 9)).unwrap().chars().count(), MAX_TITLE_CHARS);
        assert!(normalize_title("a\x1b[2Jb").is_err());
        let mut ledger = ledger();
        let id = new_message_id();
        let record = ledger.accept_with_policy(&id,address(),"hello".into(),900,false,0,"  세션 이름\n바꾸기 ".into()).unwrap();
        assert_eq!(record.title, "세션 이름 바꾸기");
        let plain = ledger.accept(&new_message_id(),address(),"plain".into(),900).unwrap();
        assert!(plain.title.is_empty() && !serde_json::to_string(&plain).unwrap().contains("\"title\""), "제목 없는 옛 꼴 그대로");
    }
    #[test] fn hold_reasons_round_trip_and_old_sentences_stay_unknown() {
        for hold in Hold::ALL { assert_eq!(Hold::from_reason(hold.reason()), Some(hold)); }
        assert_eq!(Hold::from_reason("no bytes written; waiting for fresh identity and an empty input"), None);
        assert_eq!(Hold::from_reason("waiting:nonsense — x"), None);
    }
    #[test] fn a_flag_an_old_cli_leaked_into_the_body_is_caught() {
        assert_eq!(leaked_cli_flag("⟦세이아⟧ --title 카사텀 보드 버튼·창 걷기 --stdin"), Some("--title"));
        assert_eq!(leaked_cli_flag("--stdin"), Some("--stdin"));
        assert_eq!(leaked_cli_flag("  --id kt1.1.0123456789abcdef hello"), Some("--id"));
        for fine in ["옵션 --title 은 본문 앞에", "--titles are fine", "⟦유우카⟧ 새 일", "-- 표시"] {
            assert_eq!(leaked_cli_flag(fine), None, "{fine}");
        }
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
