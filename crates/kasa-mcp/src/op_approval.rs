//! 키체인 비밀 읽기 — 폰 Face ID 한 번으로(docs/secret-faceid-approval.md). 맥 앱 프로세스 쪽.
//!
//! 학생 칸의 `kasaterm-cli secret read|run` → 소켓 → [`read`] → 관문 비밀 요청 → 폰이 Secure Enclave 키로 도전값에
//! 서명 → 여기서 도전값·믿은 키·관문의 살아 있는 키·서명·nonce·만료를 다시 본 뒤에만 이 맥의 키체인에서 값을 꺼낸다.
//! 값은 소켓으로 요청한 프로세스에만 간다. 처음 꺼내는 항목은 macOS 가 이 맥에 접근 허용 창을 띄운다.
//!
//! 이름의 `op` 는 1Password 로 읽던 때(2026-10-06~19)의 것이다 — 폰이 믿은 열쇠가 그때 이름의 키체인 항목에 있어
//! 서비스 이름(`kasaterm.1password`)은 그대로 둔다. 바꾸면 연결해 둔 폰을 다시 믿어야 한다.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approval_text::{key_fingerprint, key_id, mask, signature_ok};
use crate::device_auth::approvals as gateway;

const SERVICE: &str = "kasaterm.1password";
const TRUSTED: &str = "trusted-keys";
/// 사람이 맥에서 거둔 열쇠 id — 자동 믿기가 다시 집어 들지 않게.
const REFUSED: &str = "refused-keys";
/// 관문 원격 승인과 같은 2분.
const TTL_MS: u64 = 120_000;
const MAX_REFS: usize = 16;
const AUDIT_ROTATE: u64 = 2 * 1024 * 1024;

/// 사람이 맥 화면에서 믿은 폰 열쇠.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trusted {
    pub device: String,
    pub label: String,
    pub id: String,
    pub public: String,
    pub added: u64,
}

/// 요청한 쪽 — 소켓이 peer 프로세스로 찾은 칸·학생과, 그 프로세스가 아직 살아 있나.
pub struct Origin {
    pub pane: String,
    pub student: String,
    pub alive: Box<dyn Fn() -> bool + Send>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── 보관 ──────────────────────────────────────────────────────────────────────

/// 격리 실행(검증 리그)은 사람의 키체인을 절대 안 건드린다 — 리그가 준 폴더의 파일만 쓴다.
fn isolated_dir() -> anyhow::Result<Option<PathBuf>> {
    if crate::device_auth::sync_environment_allowed() {
        return Ok(None);
    }
    match std::env::var_os("KASATERM_OP_STORE_DIR") {
        Some(dir) => Ok(Some(PathBuf::from(dir))),
        None => bail!("isolated_run"),
    }
}

fn store_get(account: &str) -> anyhow::Result<Option<String>> {
    if let Some(dir) = isolated_dir()? {
        return Ok(std::fs::read_to_string(dir.join(account)).ok());
    }
    keychain::get(account)
}

fn store_set(account: &str, value: &str) -> anyhow::Result<()> {
    if let Some(dir) = isolated_dir()? {
        return Ok(crate::relay_auth::write_private(&dir.join(account), value)?);
    }
    keychain::set(account, value)
}

#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::passwords;

    const NOT_FOUND: i32 = -25300;

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

    /// 학생이 고른 항목의 값. 일반 암호를 먼저, 없으면 같은 이름의 인터넷 암호(서버)를 본다. 계정을 비우면 그 서비스의
    /// 아무 항목이나 잡힌다. 이 앱이 만들지 않은 항목은 macOS 가 이 맥에 접근 허용 창을 띄우고 답이 올 때까지 기다린다.
    pub fn value(service: &str, account: Option<&str>) -> anyhow::Result<Vec<u8>> {
        use security_framework::os::macos::passwords::{
            find_generic_password, find_internet_password, SecAuthenticationType, SecProtocolType,
        };
        let account = account.unwrap_or("");
        let refused = |code: i32| match code {
            USER_CANCELED | AUTH_FAILED => anyhow::anyhow!("keychain_denied"),
            code => anyhow::anyhow!("keychain: {code}"),
        };
        match find_generic_password(None, service, account) {
            Ok((password, _)) => return Ok(password.to_vec()),
            Err(error) if error.code() != NOT_FOUND => return Err(refused(error.code())),
            Err(_) => {}
        }
        match find_internet_password(None, service, None, account, "", None, SecProtocolType::Any, SecAuthenticationType::Any) {
            Ok((password, _)) => Ok(password.to_vec()),
            Err(error) if error.code() == NOT_FOUND => anyhow::bail!("not_found"),
            Err(error) => Err(refused(error.code())),
        }
    }

    const USER_CANCELED: i32 = -128;
    const AUTH_FAILED: i32 = -25293;
}

/// Windows 자격 증명 관리자의 일반 자격 증명 — 키체인 자리. 맥의 「서비스·계정」은 「대상·사용자」다
/// (`cmdkey /generic:<서비스> /user:<계정> /pass` 가 만드는 꼴). 같은 사용자의 프로세스면 묻지 않고 읽히므로
/// 맥의 접근 허용 창 같은 둘째 문은 없다 — 값은 폰 Face ID 허락 뒤에만 꺼낸다.
#[cfg(windows)]
mod keychain {
    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    /// 자격 증명 하나에 담기는 값의 상한(CRED_MAX_CREDENTIAL_BLOB_SIZE). 믿은 폰이 여럿이면 열쇠 목록이 넘으므로 뒤는
    /// `<대상>#2`, `#3` … 에 잇고, 머리의 설명 칸에 조각 수를 적는다.
    const BLOB_MAX: usize = 5 * 512;

    struct Cred {
        blob: Vec<u8>,
        user: String,
        comment: String,
    }

    fn read(target: &str) -> anyhow::Result<Option<Cred>> {
        let mut found: *mut CREDENTIALW = std::ptr::null_mut();
        match unsafe { CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut found) } {
            Ok(()) => unsafe {
                let c = &*found;
                let blob = if c.CredentialBlob.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize).to_vec()
                };
                let text = |p: PWSTR| if p.is_null() { String::new() } else { p.to_string().unwrap_or_default() };
                let cred = Cred { blob, user: text(c.UserName), comment: text(c.Comment) };
                CredFree(found as *const _);
                Ok(Some(cred))
            },
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
            Err(error) => anyhow::bail!("credential: {:#x}", error.code().0),
        }
    }

    pub(super) fn write(target: &str, user: &str, blob: &[u8], comment: &str) -> anyhow::Result<()> {
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (mut target, mut comment, mut user) = (wide(target), wide(comment), wide(user));
        let cred = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR(comment.as_mut_ptr()),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: PWSTR(user.as_mut_ptr()),
            ..Default::default()
        };
        unsafe { CredWriteW(&cred, 0) }.map_err(|error| anyhow::anyhow!("credential: {:#x}", error.code().0))
    }

    pub(super) fn delete(target: &str) {
        let _ = unsafe { CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None) };
    }

    fn target(account: &str) -> String {
        format!("{}/{account}", super::SERVICE)
    }

    fn parts(cred: &Cred) -> usize {
        cred.comment.strip_prefix("parts=").and_then(|n| n.parse().ok()).unwrap_or(1)
    }

    pub fn get(account: &str) -> anyhow::Result<Option<String>> {
        let base = target(account);
        let Some(head) = read(&base)? else { return Ok(None) };
        let mut bytes = head.blob.clone();
        for i in 2..=parts(&head) {
            let part = read(&format!("{base}#{i}"))?.ok_or_else(|| anyhow::anyhow!("credential_incomplete"))?;
            bytes.extend(part.blob);
        }
        Ok(Some(String::from_utf8(bytes)?))
    }

    /// 뒤 조각을 먼저 쓰고 머리를 맨 끝에 — 머리가 말하는 조각 수가 늘 다 써진 조각을 가리킨다.
    pub fn set(account: &str, value: &str) -> anyhow::Result<()> {
        let base = target(account);
        let before = read(&base)?.map(|c| parts(&c)).unwrap_or(1);
        let chunks: Vec<&[u8]> = if value.is_empty() { vec![&[]] } else { value.as_bytes().chunks(BLOB_MAX).collect() };
        for (i, chunk) in chunks.iter().enumerate().skip(1) {
            write(&format!("{base}#{}", i + 1), "kasaterm", chunk, "")?;
        }
        write(&base, "kasaterm", chunks[0], &format!("parts={}", chunks.len()))?;
        for i in chunks.len() + 1..=before {
            delete(&format!("{base}#{i}"));
        }
        Ok(())
    }

    /// 남이 넣은 값은 UTF-16 으로 적힌 것이 많다(cmdkey·Python keyring·Git 자격 증명 관리자). ASCII 의 UTF-16 은
    /// 늘 0 바이트를 품고, UTF-8 문자열은 0 을 안 품으니 그것으로 가른다. 끝의 NUL 은 C 문자열 꼬리라 뗀다.
    pub(super) fn decode(blob: Vec<u8>) -> Vec<u8> {
        if !blob.contains(&0) && std::str::from_utf8(&blob).is_ok() {
            return blob;
        }
        if blob.len() % 2 == 0 {
            let units: Vec<u16> = blob.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
            if let Ok(text) = String::from_utf16(&units) {
                return text.trim_end_matches('\0').as_bytes().to_vec();
            }
        }
        blob
    }

    /// 학생이 고른 항목의 값. 대상 하나에 사용자는 하나뿐이라, 같은 서비스에 계정이 여럿이면 `<계정>@<서비스>`
    /// 대상으로 둔 것도 본다(Python keyring 과 같은 꼴). 계정을 비우면 그 대상의 값이다.
    pub fn value(service: &str, account: Option<&str>) -> anyhow::Result<Vec<u8>> {
        if let Some(cred) = read(service)? {
            if account.is_none_or(|a| a == cred.user) {
                return Ok(decode(cred.blob));
            }
        }
        if let Some(account) = account {
            if let Some(cred) = read(&format!("{account}@{service}"))? {
                return Ok(decode(cred.blob));
            }
        }
        anyhow::bail!("not_found")
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod keychain {
    pub fn get(_: &str) -> anyhow::Result<Option<String>> {
        anyhow::bail!("unsupported")
    }
    pub fn set(_: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("unsupported")
    }
    pub fn value(_: &str, _: Option<&str>) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("unsupported")
    }
}

pub fn trusted() -> anyhow::Result<Vec<Trusted>> {
    Ok(store_get(TRUSTED)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
}

fn refused() -> anyhow::Result<Vec<String>> {
    Ok(store_get(REFUSED)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
}

/// 보관한 것이 바뀌었다 — 화면은 다음 poll 에 1분 주기를 기다리지 않고 다시 읽는다(소켓 [`sync`] 가 바꾼 것도).
static CHANGED: AtomicBool = AtomicBool::new(false);

pub fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::Relaxed)
}

// ── 감사 ──────────────────────────────────────────────────────────────────────

fn audit_path() -> Option<PathBuf> {
    match isolated_dir() {
        Ok(Some(dir)) => Some(dir.join("op-audit.jsonl")),
        Ok(None) => Some(kasa_socket::home_dir()?.join(".config/kasaterm/op-audit.jsonl")),
        Err(_) => None,
    }
}

/// 값·토큰은 안 적는다 — 누가·언제·어느 칸에서·어떤 참조를·누가 허락했고 결과가 무엇인지만.
fn audit(line: Value) {
    let Some(path) = audit_path() else { return };
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > AUDIT_ROTATE) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    if let Ok(mut file) = options.open(&path) {
        let mut line = line;
        line["at"] = json!(now_ms());
        let _ = writeln!(file, "{line}");
    }
}

// ── 참조·도전값 ───────────────────────────────────────────────────────────────

/// 참조 꼴 — `keychain://<서비스>` 또는 `keychain://<서비스>#<계정>`. 서비스 이름에 `#` 이 들어갈 수 있어 계정은
/// 마지막 `#` 뒤다.
pub const REF_PREFIX: &str = "keychain://";

pub fn parse_ref(reference: &str) -> Option<(&str, Option<&str>)> {
    let rest = reference.strip_prefix(REF_PREFIX)?;
    let (service, account) = match rest.rsplit_once('#') {
        Some((service, account)) => (service, Some(account)),
        None => (rest, None),
    };
    (!service.is_empty() && account.is_none_or(|a| !a.is_empty())).then_some((service, account))
}

fn check_refs(refs: &[String]) -> anyhow::Result<()> {
    if refs.is_empty() || refs.len() > MAX_REFS {
        bail!("bad_ref");
    }
    for reference in refs {
        if reference.len() > 256 || reference.chars().any(char::is_control) || parse_ref(reference).is_none() {
            bail!("bad_ref");
        }
    }
    Ok(())
}

/// 허락이 난 뒤에 값을 꺼낸다. 격리 실행은 리그 폴더의 `keychain.json`(`{"서비스#계정" 또는 "서비스": 값}`)만 본다.
fn resolve(refs: &[String]) -> anyhow::Result<Vec<String>> {
    let fake: Option<Value> = isolated_dir()?
        .map(|dir| std::fs::read(dir.join("keychain.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
    refs.iter()
        .map(|reference| {
            let (service, account) = parse_ref(reference).ok_or_else(|| anyhow!("bad_ref"))?;
            let bytes = match &fake {
                Some(map) => {
                    let key = account.map_or_else(|| service.to_string(), |a| format!("{service}#{a}"));
                    map[&key].as_str().map(|v| v.as_bytes().to_vec()).ok_or_else(|| anyhow!("not_found"))?
                }
                None => keychain::value(service, account)?,
            };
            String::from_utf8(bytes).map_err(|_| anyhow!("not_text"))
        })
        .collect()
}

fn clip(text: &str, max_chars: usize) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max_chars).collect()
}

fn clip_bytes(text: &str, max: usize) -> String {
    let mut out = text.to_string();
    let mut cut = out.len().min(max);
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    out
}

#[allow(clippy::too_many_arguments)]
fn challenge(account: &str, device: &str, nonce: &str, exp: u64, origin: &Origin, cwd: &str, command: &str, refs: &[String]) -> String {
    json!({
        "v": 1,
        "kind": "keychain.read",
        "nonce": nonce,
        "account": account,
        "device": device,
        "student": clip(&origin.student, 64),
        "pane": clip(&origin.pane, 64),
        "cwd": clip(cwd, 1024),
        // 명령 줄은 사람에게 보이는 글이라 다른 비밀(헤더 토큰 따위)을 가린다. 참조는 따로 실린다.
        "command": clip_bytes(&mask(command), 2048),
        "refs": refs,
        "exp": exp,
    })
    .to_string()
}

fn nonce() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).expect("system random");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── 결정 확인 ─────────────────────────────────────────────────────────────────

/// 이 앱이 올린 요청.
struct Sent {
    id: String,
    challenge: String,
    nonce: String,
    exp: u64,
}

/// 쓴 nonce — 같은 허락을 두 번 쓰지 않게. 만료가 지난 것은 걷는다.
#[derive(Default)]
struct Ledger(HashMap<String, u64>);

impl Ledger {
    fn consume(&mut self, nonce: &str, exp: u64, now: u64) -> bool {
        self.0.retain(|_, until| *until > now);
        self.0.insert(nonce.to_string(), exp).is_none()
    }
}

static LEDGER: std::sync::LazyLock<Mutex<Ledger>> = std::sync::LazyLock::new(Mutex::default);

/// 관문이 「허락됨」이라 한 결정을 맥이 다시 본다. 관문이 혼자 허락을 지어낼 수 없게 — 사람이 이 맥에서 믿은
/// 키여야 하고, 그 키가 관문 목록에도 살아 있어야 하고(폐기된 폰은 빠진다), 서명이 올린 도전값 그대로에 맞아야 한다.
fn check(sent: &Sent, view: &Value, trusted: &[Trusted], live: &[Value], ledger: &mut Ledger, now: u64) -> Result<String, &'static str> {
    match view["state"].as_str() {
        Some("allowed") => {}
        Some("denied") => return Err("denied"),
        Some("expired") => return Err("expired"),
        Some("cancelled") => return Err("cancelled"),
        _ => return Err("not_decided"),
    }
    if view["id"].as_str() != Some(sent.id.as_str()) {
        return Err("request_changed");
    }
    if view["challenge"].as_str() != Some(sent.challenge.as_str()) {
        return Err("challenge_changed");
    }
    if now >= sent.exp {
        return Err("expired");
    }
    let (Some(key), Some(sig)) = (view["key"].as_str(), view["sig"].as_str()) else {
        return Err("signature_missing");
    };
    let device = view["by"]["device"].as_str().unwrap_or_default();
    let Some(t) = trusted.iter().find(|t| t.id == key && t.device == device) else {
        return Err("key_not_trusted");
    };
    let alive = live.iter().any(|k| k["id"] == key && k["device"] == device && k["public"] == t.public.as_str());
    if !alive {
        return Err("key_revoked");
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(public), Ok(raw)) = (engine.decode(&t.public), engine.decode(sig)) else {
        return Err("bad_signature");
    };
    if !signature_ok(&public, &sent.challenge, &raw) {
        return Err("bad_signature");
    }
    if !ledger.consume(&sent.nonce, sent.exp, now) {
        return Err("replayed");
    }
    Ok(t.label.clone())
}

// ── 소켓 쪽 ───────────────────────────────────────────────────────────────────

/// 학생 칸의 요청 하나. 폰에서 사람이 Face ID 로 허락하면 참조 순서대로 값을 돌려준다.
pub fn read(refs: &[String], command: &str, cwd: &str, origin: &Origin) -> anyhow::Result<Vec<String>> {
    check_refs(refs)?;
    let mut trusted = trusted()?;
    if trusted.is_empty() {
        // 폰이 막 「연결」했으면 화면의 1분 주기 전이라도 여기서 믿는다.
        let _ = sync();
        trusted = self::trusted()?;
    }
    if trusted.is_empty() {
        bail!("no_trusted_key");
    }
    let (account, device) = gateway::identity()?;
    let exp = now_ms() + TTL_MS;
    let nonce = nonce();
    let text = challenge(&account, &device, &nonce, exp, origin, cwd, command, refs);
    let view = gateway::create_secret_blocking(&text)?;
    let sent = Sent { id: view["id"].as_str().unwrap_or_default().to_string(), challenge: text, nonce, exp };
    let base = json!({"id": sent.id, "student": origin.student, "pane": origin.pane, "cwd": cwd, "refs": refs});
    let mut line = base.clone();
    line["event"] = json!("requested");
    audit(line);
    let finish = |event: &str, extra: Value| {
        let mut line = base.clone();
        line["event"] = json!(event);
        if let Value::Object(map) = extra {
            for (k, v) in map {
                line[k] = v;
            }
        }
        audit(line);
    };
    let mut misses = 0;
    let decided = loop {
        if !(origin.alive)() {
            let _ = gateway::cancel_blocking(&sent.id, "gone");
            finish("cancelled", json!({"reason": "requester_gone"}));
            bail!("cancelled");
        }
        if now_ms() > sent.exp + 5_000 {
            finish("expired", json!({}));
            bail!("expired");
        }
        match gateway::wait_one_blocking(&sent.id, 5) {
            Ok(view) if view["state"] == "pending" => misses = 0,
            Ok(view) => break view,
            Err(error) => {
                if let Some(view) = gateway::closed_view(&error) {
                    break view;
                }
                misses += 1;
                if misses > 8 {
                    finish("failed", json!({"reason": "gateway_unreachable"}));
                    bail!("gateway_unreachable");
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    };
    let live = gateway::keys_blocking().unwrap_or_default();
    let by = json!({"device": decided["by"]["device"], "label": decided["by"]["label"], "key": decided["key"]});
    let checked = check(&sent, &decided, &trusted, &live, &mut LEDGER.lock().unwrap(), now_ms());
    if let Err(reason) = checked {
        finish(if reason == "denied" { "denied" } else { "refused" }, json!({"reason": reason, "by": by}));
        bail!("{reason}");
    }
    let started = Instant::now();
    match resolve(refs) {
        Ok(values) => {
            finish("resolved", json!({"by": by, "ms": started.elapsed().as_millis() as u64}));
            Ok(values)
        }
        Err(error) => {
            let reason = error.to_string();
            finish("failed", json!({"reason": clip(&reason, 300), "by": by}));
            Err(error)
        }
    }
}

/// 믿는 폰 열쇠 — 값은 없다. 소켓에도 열려 있다. `token` 은 옛 폰 앱이 「준비됨」으로 읽는 칸이라 늘 참으로 둔다
/// (키체인은 따로 넣을 것이 없다).
pub fn status() -> Value {
    match trusted() {
        Ok(list) => json!({
            "source": "keychain",
            "token": true,
            "trusted": list.iter().map(|t| json!({"label": t.label, "fingerprint": key_fingerprint(&t.id)})).collect::<Vec<_>>(),
        }),
        Err(error) => json!({"source": "keychain", "token": false, "error": error.to_string()}),
    }
}

// ── 앱 화면 쪽(소켓에 없다) ───────────────────────────────────────────────────

fn same_key(k: &Value, t: &Trusted) -> bool {
    k["id"] == t.id.as_str() && k["device"] == t.device.as_str()
}

fn with_fingerprint(mut k: Value) -> Value {
    let id = k["id"].as_str().unwrap_or_default().to_string();
    k["fingerprint"] = json!(key_fingerprint(&id));
    k
}

/// 사람 확인 없이 믿을 폰 열쇠(2026-10-13 결정 — 연결은 폰 Face ID 한 번으로 끝난다).
/// 이미 믿은 기기가 갈아 끼운 열쇠(Face ID 재등록)는 언제나 믿는다. 그런 열쇠가 없고 이 맥이 믿는 열쇠 중 관문에 살아
/// 있는 것도 없으면 들어온 열쇠를 다 믿는다(처음 연결). 그 밖의 새 기기는 맥 화면의 「믿기」로 남긴다.
/// 사람이 맥에서 거둔 열쇠는 다시 집지 않는다. 관문이 열쇠를 끼워 넣으면 막지 못한다는 것을 받아들인 규칙이다.
pub fn auto_pick(trusted: &[Trusted], live: &[Value], refused: &[String]) -> Vec<Value> {
    let alive = trusted.iter().any(|t| live.iter().any(|k| same_key(k, t)));
    let fresh: Vec<Value> = live
        .iter()
        .filter(|k| !trusted.iter().any(|t| same_key(k, t)))
        .filter(|k| !refused.iter().any(|id| k["id"] == id.as_str()))
        .cloned()
        .collect();
    let renewed: Vec<Value> = fresh.iter().filter(|k| trusted.iter().any(|t| k["device"] == t.device.as_str())).cloned().collect();
    if alive || !renewed.is_empty() { renewed } else { fresh }
}

/// 관문 열쇠 목록을 한 번 보고 [`auto_pick`] 이 고른 열쇠를 믿은 뒤, 지금 상태와 아직 사람이 믿어야 할 열쇠를 준다.
pub fn sync() -> anyhow::Result<(Value, Vec<Value>)> {
    let live = gateway::keys_blocking()?;
    for key in auto_pick(&trusted()?, &live, &refused()?) {
        keep(&key, "key_auto_trusted")?;
    }
    let trusted = trusted()?;
    let rest = live.into_iter().filter(|k| !trusted.iter().any(|t| same_key(k, t))).map(with_fingerprint).collect();
    Ok((status(), rest))
}

/// 사람이 맥 화면에서 누른 「믿기」.
pub fn trust(key: &Value) -> anyhow::Result<()> {
    keep(key, "key_trusted")
}

/// 공개키가 그 id 의 키인지 다시 보고 믿는다. 같은 기기의 옛 열쇠는 갈아 끼운다.
fn keep(key: &Value, event: &str) -> anyhow::Result<()> {
    let public = key["public"].as_str().ok_or_else(|| anyhow!("bad_key"))?;
    let raw = base64::engine::general_purpose::STANDARD.decode(public).map_err(|_| anyhow!("bad_key"))?;
    let id = key["id"].as_str().unwrap_or_default();
    anyhow::ensure!(raw.len() == 65 && raw[0] == 4 && key_id(&raw) == id, "bad_key");
    let device = key["device"].as_str().unwrap_or_default().to_string();
    let mut list = trusted()?;
    list.retain(|t| t.device != device);
    list.push(Trusted {
        device: device.clone(),
        label: key["label"].as_str().unwrap_or("폰").to_string(),
        id: id.to_string(),
        public: public.to_string(),
        added: now_ms(),
    });
    store_set(TRUSTED, &serde_json::to_string(&list)?)?;
    let mut no = refused()?;
    if no.iter().any(|r| r == id) {
        no.retain(|r| r != id);
        store_set(REFUSED, &serde_json::to_string(&no)?)?;
    }
    CHANGED.store(true, Ordering::Relaxed);
    audit(json!({"event": event, "device": device, "key": id}));
    Ok(())
}

pub fn untrust(id: &str) -> anyhow::Result<()> {
    let mut list = trusted()?;
    list.retain(|t| t.id != id);
    store_set(TRUSTED, &serde_json::to_string(&list)?)?;
    let mut no = refused()?;
    if !no.iter().any(|r| r == id) {
        no.push(id.to_string());
        let keep = no.len().saturating_sub(64);
        store_set(REFUSED, &serde_json::to_string(&no[keep..])?)?;
    }
    CHANGED.store(true, Ordering::Relaxed);
    audit(json!({"event": "key_untrusted", "key": id}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Phone(ring::signature::EcdsaKeyPair);

    impl Phone {
        fn new() -> Self {
            let rng = ring::rand::SystemRandom::new();
            let alg = &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING;
            let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
            Self(ring::signature::EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap())
        }
        fn public(&self) -> String {
            use ring::signature::KeyPair as _;
            base64::engine::general_purpose::STANDARD.encode(self.0.public_key().as_ref())
        }
        fn id(&self) -> String {
            use ring::signature::KeyPair as _;
            key_id(self.0.public_key().as_ref())
        }
        fn sign(&self, text: &str) -> String {
            let mut message = crate::approval_text::SECRET_PREFIX.to_vec();
            message.extend_from_slice(text.as_bytes());
            let sig = self.0.sign(&ring::rand::SystemRandom::new(), &message).unwrap();
            base64::engine::general_purpose::STANDARD.encode(sig.as_ref())
        }
        fn trusted(&self) -> Trusted {
            Trusted { device: "p1".into(), label: "폰".into(), id: self.id(), public: self.public(), added: 0 }
        }
        fn live(&self) -> Value {
            json!({"device": "p1", "label": "폰", "id": self.id(), "public": self.public()})
        }
    }

    fn origin() -> Origin {
        Origin { pane: "%3".into(), student: "유우카".into(), alive: Box::new(|| true) }
    }

    fn sent(nonce: &str) -> Sent {
        let refs = vec!["keychain://db.example#deploy".to_string()];
        let text = challenge("geno", "book", nonce, 100_000, &origin(), "/repo", "kasaterm-cli secret read keychain://db.example#deploy", &refs);
        Sent { id: "apv_1".into(), challenge: text, nonce: nonce.into(), exp: 100_000 }
    }

    fn allowed(sent: &Sent, phone: &Phone, sig: &str) -> Value {
        json!({"id": sent.id, "state": "allowed", "challenge": sent.challenge, "key": phone.id(), "sig": sig,
            "by": {"device": "p1", "label": "폰", "kind": "phone"}})
    }

    #[test]
    fn a_signed_allow_from_a_trusted_live_key_passes_once() {
        let phone = Phone::new();
        let s = sent("00000000000000000000000000000001");
        let view = allowed(&s, &phone, &phone.sign(&s.challenge));
        let mut ledger = Ledger::default();
        assert_eq!(check(&s, &view, &[phone.trusted()], &[phone.live()], &mut ledger, 1_000), Ok("폰".into()));
        // 같은 허락을 한 번 더 — nonce 재사용.
        assert_eq!(check(&s, &view, &[phone.trusted()], &[phone.live()], &mut ledger, 1_000), Err("replayed"));
    }

    #[test]
    fn the_mac_refuses_what_the_gateway_alone_could_forge() {
        let phone = Phone::new();
        let s = sent("00000000000000000000000000000002");
        let good = phone.sign(&s.challenge);
        let mut ledger = Ledger::default();
        let mut run = |view: &Value, trusted: &[Trusted], live: &[Value], now: u64| check(&s, view, trusted, live, &mut ledger, now);

        // 관문이 도전값을 바꿔치기(변조).
        let mut swapped = allowed(&s, &phone, &good);
        swapped["challenge"] = json!(s.challenge.replace("db.example#deploy", "db.example#other"));
        assert_eq!(run(&swapped, &[phone.trusted()], &[phone.live()], 1_000), Err("challenge_changed"));
        // 사람이 이 맥에서 믿지 않은 키(관문이 끼워 넣은 키).
        let stranger = Phone::new();
        let forged = allowed(&s, &stranger, &stranger.sign(&s.challenge));
        assert_eq!(run(&forged, &[phone.trusted()], &[phone.live(), stranger.live()], 1_000), Err("key_not_trusted"));
        // 믿은 키지만 관문에서 폐기돼 목록에 없다.
        assert_eq!(run(&allowed(&s, &phone, &good), &[phone.trusted()], &[], 1_000), Err("key_revoked"));
        // 서명이 다른 글에 대한 것.
        let wrong = allowed(&s, &phone, &phone.sign("다른 글"));
        assert_eq!(run(&wrong, &[phone.trusted()], &[phone.live()], 1_000), Err("bad_signature"));
        let mut unsigned = allowed(&s, &phone, &good);
        unsigned["sig"] = Value::Null;
        assert_eq!(run(&unsigned, &[phone.trusted()], &[phone.live()], 1_000), Err("signature_missing"));
        // 만료 뒤.
        assert_eq!(run(&allowed(&s, &phone, &good), &[phone.trusted()], &[phone.live()], 100_000), Err("expired"));
        // 거절·취소·만료 상태는 그대로 거절.
        for (state, reason) in [("denied", "denied"), ("cancelled", "cancelled"), ("expired", "expired"), ("pending", "not_decided")] {
            let mut view = allowed(&s, &phone, &good);
            view["state"] = json!(state);
            assert_eq!(run(&view, &[phone.trusted()], &[phone.live()], 1_000), Err(reason));
        }
        // 다른 요청의 허락.
        let mut other = allowed(&s, &phone, &good);
        other["id"] = json!("apv_2");
        assert_eq!(run(&other, &[phone.trusted()], &[phone.live()], 1_000), Err("request_changed"));
        // 위 거절들은 nonce 를 쓰지 않는다 — 진짜 허락은 여전히 한 번 된다.
        assert!(run(&allowed(&s, &phone, &good), &[phone.trusted()], &[phone.live()], 1_000).is_ok());
    }

    #[test]
    fn keychain_refs_name_a_service_and_maybe_an_account() {
        assert_eq!(parse_ref("keychain://github.com"), Some(("github.com", None)));
        assert_eq!(parse_ref("keychain://db#me@x.com"), Some(("db", Some("me@x.com"))));
        assert_eq!(parse_ref("keychain://a#b#c"), Some(("a#b", Some("c"))), "계정은 마지막 # 뒤");
        for bad in ["keychain://", "keychain://#me", "keychain://svc#", "op://vault/item/field", "github.com"] {
            assert!(parse_ref(bad).is_none(), "{bad}");
        }
        let ok = |r: &[&str]| check_refs(&r.iter().map(|x| x.to_string()).collect::<Vec<_>>()).map_err(|e| e.to_string());
        assert!(ok(&["keychain://db#deploy", "keychain://api"]).is_ok());
        assert_eq!(ok(&[]).unwrap_err(), "bad_ref");
        assert_eq!(ok(&["op://kasaterm-agents/db/password"]).unwrap_err(), "bad_ref", "1Password 참조는 더 안 받는다");
        assert_eq!(ok(&["keychain://a\nb"]).unwrap_err(), "bad_ref");
    }

    #[test]
    fn the_challenge_masks_other_secrets_in_the_command_but_keeps_the_refs() {
        let refs = vec!["keychain://db.example#deploy".to_string()];
        let text = challenge("geno", "book", "n", 9, &origin(), "/repo",
            "kasaterm-cli secret run -e DB=keychain://db.example#deploy -- curl -H 'Authorization: Bearer sk-live-abcdefghijklmnop'", &refs);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(!v["command"].as_str().unwrap().contains("sk-live"), "{}", v["command"]);
        assert_eq!(v["refs"][0], "keychain://db.example#deploy");
        assert_eq!(v["kind"], "keychain.read");
        assert_eq!(v["student"], "유우카");
    }

    #[test]
    fn the_first_phone_is_trusted_without_a_person_but_a_second_device_is_not() {
        let (first, second) = (Phone::new(), Phone::new());
        let mut second_live = second.live();
        second_live["device"] = json!("p2");
        // 처음 연결 — 믿는 열쇠가 없으니 들어온 것을 다 믿는다.
        assert_eq!(auto_pick(&[], &[first.live()], &[]), vec![first.live()]);
        // 첫 폰이 살아 있으면 다른 기기는 사람이 맥에서 믿는다.
        assert!(auto_pick(&[first.trusted()], &[first.live(), second_live.clone()], &[]).is_empty());
        // 같은 기기가 Face ID 를 다시 등록해 열쇠를 갈아 끼우면 새 열쇠를 믿는다.
        let renewed = Phone::new();
        assert_eq!(auto_pick(&[first.trusted()], &[renewed.live(), second_live.clone()], &[]), vec![renewed.live()]);
        // 믿던 열쇠가 관문에서 모두 사라졌으면 다시 처음 연결이다.
        assert_eq!(auto_pick(&[first.trusted()], &[second_live.clone()], &[]), vec![second_live.clone()]);
        // 사람이 맥에서 거둔 열쇠는 다시 집지 않는다.
        assert!(auto_pick(&[], &[first.live()], &[first.id()]).is_empty());
    }

    /// 손으로 돌리는 시험 — `security add-generic-password -A -s kasaterm.probe -a probe -w probe-value` 로 넣은 항목을 읽는다
    /// (`-A` 라 허용 창이 안 뜬다). `cargo test -p kasa-mcp real_keychain -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn a_real_keychain_item_is_read_by_service_and_account() {
        assert_eq!(keychain::value("kasaterm.probe", Some("probe")).unwrap(), b"probe-value");
        assert_eq!(keychain::value("kasaterm.probe", None).unwrap(), b"probe-value");
        assert_eq!(keychain::value("kasaterm.probe", Some("nobody")).unwrap_err().to_string(), "not_found");
        assert_eq!(keychain::value("kasaterm.no-such-service", None).unwrap_err().to_string(), "not_found");
    }

    #[test]
    #[cfg(windows)]
    fn credential_values_written_as_utf16_read_back_as_text() {
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        assert_eq!(keychain::decode(b"plain-utf8".to_vec()), b"plain-utf8");
        assert_eq!(keychain::decode(utf16("probe-value")), b"probe-value");
        assert_eq!(keychain::decode(utf16("비밀\0")), "비밀".as_bytes());
        assert_eq!(keychain::decode("한글 값".as_bytes().to_vec()), "한글 값".as_bytes());
    }

    /// 손으로 돌리는 시험 — 사람의 자격 증명 관리자에 `kasaterm.probe*` 항목을 넣었다 지운다.
    /// `cargo test -p kasa-mcp real_credential -- --ignored`.
    #[test]
    #[ignore]
    #[cfg(windows)]
    fn a_real_credential_is_read_by_target_and_user_and_long_values_span_parts() {
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        keychain::write("kasaterm.probe", "probe", &utf16("probe-value"), "").unwrap();
        keychain::write("other@kasaterm.probe", "other", b"other-value", "").unwrap();
        assert_eq!(keychain::value("kasaterm.probe", Some("probe")).unwrap(), b"probe-value");
        assert_eq!(keychain::value("kasaterm.probe", None).unwrap(), b"probe-value");
        assert_eq!(keychain::value("kasaterm.probe", Some("other")).unwrap(), b"other-value");
        assert_eq!(keychain::value("kasaterm.probe", Some("nobody")).unwrap_err().to_string(), "not_found");
        assert_eq!(keychain::value("kasaterm.no-such-service", None).unwrap_err().to_string(), "not_found");
        keychain::delete("kasaterm.probe");
        keychain::delete("other@kasaterm.probe");
        // 사람이 넣는 길(docs/secret-faceid-approval.md) 그대로.
        let made = std::process::Command::new("cmdkey")
            .args(["/generic:kasaterm.probe-cmdkey", "/user:probe", "/pass:probe-value"])
            .output()
            .unwrap();
        assert!(made.status.success());
        assert_eq!(keychain::value("kasaterm.probe-cmdkey", Some("probe")).unwrap(), b"probe-value");
        keychain::delete("kasaterm.probe-cmdkey");

        let long: String = (0..6000).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        keychain::set("probe-parts", &long).unwrap();
        assert_eq!(keychain::get("probe-parts").unwrap().as_deref(), Some(long.as_str()));
        keychain::set("probe-parts", "짧은 값").unwrap();
        assert_eq!(keychain::get("probe-parts").unwrap().as_deref(), Some("짧은 값"));
        assert!(keychain::value(&format!("{SERVICE}/probe-parts#2"), None).is_err(), "a stale tail part was left behind");
        keychain::delete(&format!("{SERVICE}/probe-parts"));
        assert_eq!(keychain::get("probe-parts").unwrap(), None);
    }

    #[test]
    fn a_trusted_key_must_hash_to_its_id() {
        let phone = Phone::new();
        let mut key = phone.live();
        key["id"] = json!("0000");
        // 격리 저장소 없이 부르면 키체인 대신 거절된다 — 시험이 사람의 키체인을 건드리지 않는다.
        assert_eq!(trust(&key).unwrap_err().to_string(), "bad_key");
        assert_eq!(trust(&phone.live()).unwrap_err().to_string(), "isolated_run");
    }
}
