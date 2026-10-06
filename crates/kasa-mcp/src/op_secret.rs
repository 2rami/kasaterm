//! 1Password 비밀 읽기 — 폰 Face ID 한 번으로(docs/op-faceid-approval.md). 맥 앱 프로세스 쪽.
//!
//! 학생 칸의 `kasaterm-cli op read|run` → 소켓 → [`read`] → 관문 비밀 요청 → 폰이 Secure Enclave 키로 도전값에
//! 서명 → 여기서 도전값·믿은 키·관문의 살아 있는 키·서명·nonce·만료를 다시 본 뒤에만 실행기(`kasa-op`)에 토큰을
//! 파이프로 넘겨 값을 받는다. 값은 소켓으로 요청한 프로세스에만 간다.
//!
//! 토큰 넣기·지우기와 폰 열쇠 믿기는 앱 화면 코드만 부른다 — 소켓에는 [`read`] 와 [`status`] 뿐이다.
//! 토큰은 로그인 키체인(접근: 이 앱)에만 있고 대화·로그·감사 줄 어디에도 안 적는다.

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::approval_text::{key_fingerprint, key_id, mask, signature_ok};
use crate::device_auth::approvals as gateway;

const SERVICE: &str = "kasaterm.1password";
const SETTINGS: &str = "service-account";
const TRUSTED: &str = "trusted-keys";
/// 관문 원격 승인과 같은 2분.
const TTL_MS: u64 = 120_000;
const MAX_REFS: usize = 16;
const HELPER_TIMEOUT: Duration = Duration::from_secs(40);
const AUDIT_ROTATE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    token: String,
    vault_id: String,
    vault_name: String,
    added: u64,
}

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

fn store_delete(account: &str) -> anyhow::Result<()> {
    if let Some(dir) = isolated_dir()? {
        let _ = std::fs::remove_file(dir.join(account));
        return Ok(());
    }
    keychain::delete(account)
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

    pub fn delete(account: &str) -> anyhow::Result<()> {
        match passwords::delete_generic_password(super::SERVICE, account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == NOT_FOUND => Ok(()),
            Err(error) => anyhow::bail!("keychain: {}", error.code()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod keychain {
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

fn settings() -> anyhow::Result<Option<Settings>> {
    Ok(store_get(SETTINGS)?.and_then(|text| serde_json::from_str(&text).ok()))
}

pub fn trusted() -> anyhow::Result<Vec<Trusted>> {
    Ok(store_get(TRUSTED)?.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default())
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

/// `op://<금고>/<항목>/[<섹션>/]<필드>[?attribute=…]` 의 금고.
fn vault_of(reference: &str) -> Option<&str> {
    let rest = reference.strip_prefix("op://")?;
    let (vault, tail) = rest.split_once('/')?;
    let segments = tail.split('?').next()?.split('/').count();
    (!vault.is_empty() && (2..=3).contains(&segments) && !tail.split('/').any(str::is_empty)).then_some(vault)
}

fn check_refs(refs: &[String], s: &Settings) -> anyhow::Result<()> {
    if refs.is_empty() || refs.len() > MAX_REFS {
        bail!("bad_ref");
    }
    for reference in refs {
        if reference.len() > 256 || reference.chars().any(char::is_control) {
            bail!("bad_ref");
        }
        let vault = vault_of(reference).ok_or_else(|| anyhow!("bad_ref"))?;
        if vault != s.vault_id && !vault.eq_ignore_ascii_case(&s.vault_name) {
            bail!("vault_not_allowed");
        }
    }
    Ok(())
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
        "kind": "op.read",
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

// ── 실행기 ────────────────────────────────────────────────────────────────────

/// 실행기 자리와, 서명을 볼지. 검증 리그가 `KASATERM_OP_HELPER` 로 가짜 실행기를 줄 때만 서명을 안 본다.
fn helper_path() -> anyhow::Result<(PathBuf, bool)> {
    if isolated_dir()?.is_some() {
        if let Some(path) = std::env::var_os("KASATERM_OP_HELPER") {
            return Ok((PathBuf::from(path), false));
        }
    }
    let exe = std::env::current_exe()?;
    let path = exe.with_file_name("kasa-op");
    anyhow::ensure!(path.is_file(), "helper_missing");
    Ok((path, true))
}

/// 이 앱의 서명 팀 — 실행기는 같은 팀이 `kasa-op` 이름으로 서명한 것이어야 토큰을 받는다.
#[cfg(target_os = "macos")]
fn own_team() -> Option<String> {
    static TEAM: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TEAM.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        let out = std::process::Command::new("/usr/bin/codesign").arg("-dv").arg(&exe).output().ok()?;
        let text = String::from_utf8_lossy(&out.stderr);
        let team = text.lines().find_map(|l| l.strip_prefix("TeamIdentifier="))?.trim().to_string();
        (team.len() == 10 && team.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(team)
    })
    .clone()
}

/// 띄운 실행기 프로세스(아직 거두지 않은 우리 자식이라 pid 가 바뀌지 않는다)의 코드 서명을 본다.
#[cfg(target_os = "macos")]
fn verify_helper(pid: u32) -> anyhow::Result<()> {
    verify_helper_team(pid, &own_team().ok_or_else(|| anyhow!("helper_unverified"))?)
}

#[cfg(target_os = "macos")]
fn verify_helper_team(pid: u32, team: &str) -> anyhow::Result<()> {
    use security_framework::os::macos::code_signing::{Flags, GuestAttributes, SecCode, SecRequirement};
    let requirement: SecRequirement =
        format!("identifier \"kasa-op\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\"")
            .parse()
            .map_err(|_| anyhow!("helper_unverified"))?;
    let mut attrs = GuestAttributes::new();
    attrs.set_pid(pid as libc::pid_t);
    let code = SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE).map_err(|_| anyhow!("helper_unverified"))?;
    code.check_validity(Flags::NONE, &requirement).map_err(|_| anyhow!("helper_unverified"))
}

#[cfg(not(target_os = "macos"))]
fn verify_helper(_pid: u32) -> anyhow::Result<()> {
    bail!("unsupported")
}

/// 실행기에 한 번 묻는다. 토큰은 서명 확인 뒤 표준 입력으로만 — 환경·인자에 없다.
fn run_helper(op: &str, token: &str, refs: &[String]) -> anyhow::Result<Value> {
    let (path, verify) = helper_path()?;
    let mut child = std::process::Command::new(&path)
        .env_clear()
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| anyhow!("helper_missing"))?;
    if verify {
        if let Err(error) = verify_helper(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    }
    let input = json!({"op": op, "token": token, "refs": refs}).to_string();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    drop(input);
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("helper_failed"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(4 * 1024 * 1024).read_to_end(&mut out);
        let _ = tx.send(out);
    });
    let out = match rx.recv_timeout(HELPER_TIMEOUT) {
        Ok(out) => out,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("helper_timeout");
        }
    };
    let _ = child.wait();
    let reply: Value = serde_json::from_slice(&out).map_err(|_| anyhow!("helper_failed"))?;
    if reply["ok"] != true {
        let reason = reply["error"].as_str().unwrap_or("unknown");
        bail!("resolve_failed: {}", clip(reason, 300));
    }
    Ok(reply)
}

// ── 소켓 쪽 ───────────────────────────────────────────────────────────────────

/// 학생 칸의 요청 하나. 폰에서 사람이 Face ID 로 허락하면 참조 순서대로 값을 돌려준다.
pub fn read(refs: &[String], command: &str, cwd: &str, origin: &Origin) -> anyhow::Result<Vec<String>> {
    let s = settings()?.ok_or_else(|| anyhow!("op_token_missing"))?;
    check_refs(refs, &s)?;
    let trusted = trusted()?;
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
    match run_helper("resolve", &s.token, refs) {
        Ok(reply) => {
            let values: Vec<String> =
                reply["values"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
            if values.len() != refs.len() {
                finish("failed", json!({"reason": "helper_failed", "by": by}));
                bail!("helper_failed");
            }
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

/// 토큰 유무·허용 금고·믿는 열쇠 — 값과 토큰은 없다. 소켓에도 열려 있다.
pub fn status() -> Value {
    let s = settings();
    let helper = helper_path().is_ok();
    match s {
        Ok(s) => json!({
            "token": s.is_some(),
            "vault": s.as_ref().map(|s| s.vault_name.clone()),
            "trusted": trusted().unwrap_or_default().iter().map(|t| json!({"label": t.label, "fingerprint": key_fingerprint(&t.id)})).collect::<Vec<_>>(),
            "helper": helper,
        }),
        Err(error) => json!({"token": false, "error": error.to_string(), "helper": helper}),
    }
}

// ── 앱 화면 쪽(소켓에 없다) ───────────────────────────────────────────────────

/// 사람이 앱 화면의 암호 칸에 넣은 토큰. 실행기로 그 토큰이 보는 금고를 물어, 금고가 **하나**일 때만 받는다.
pub fn set_token(token: &str) -> anyhow::Result<String> {
    let token = token.trim();
    anyhow::ensure!(token.starts_with("ops_") && token.len() < 8192 && !token.chars().any(char::is_whitespace), "token_invalid");
    let reply = run_helper("vaults", token, &[])?;
    let vaults = reply["vaults"].as_array().cloned().unwrap_or_default();
    if vaults.len() != 1 {
        bail!("vault_count:{}", vaults.len());
    }
    let name = vaults[0]["name"].as_str().unwrap_or_default().to_string();
    let s = Settings {
        token: token.to_string(),
        vault_id: vaults[0]["id"].as_str().unwrap_or_default().to_string(),
        vault_name: name.clone(),
        added: now_ms(),
    };
    store_set(SETTINGS, &serde_json::to_string(&s)?)?;
    audit(json!({"event": "token_set", "vault": name}));
    Ok(name)
}

pub fn clear_token() -> anyhow::Result<()> {
    store_delete(SETTINGS)?;
    audit(json!({"event": "token_cleared"}));
    Ok(())
}

/// 관문에 맡겨졌지만 이 맥이 아직 안 믿은 폰 열쇠 — 화면이 지문과 함께 「믿기」를 묻는다.
pub fn untrusted_keys() -> anyhow::Result<Vec<Value>> {
    let trusted = trusted()?;
    let live = gateway::keys_blocking()?;
    Ok(live
        .into_iter()
        .filter(|k| !trusted.iter().any(|t| k["id"] == t.id.as_str() && k["device"] == t.device.as_str()))
        .map(|mut k| {
            let id = k["id"].as_str().unwrap_or_default().to_string();
            k["fingerprint"] = json!(key_fingerprint(&id));
            k
        })
        .collect())
}

/// 사람이 맥 화면에서 지문을 맞춰 보고 누른 「믿기」. 공개키가 그 id 의 키인지 다시 본다.
pub fn trust(key: &Value) -> anyhow::Result<()> {
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
    audit(json!({"event": "key_trusted", "device": device, "key": id}));
    Ok(())
}

pub fn untrust(id: &str) -> anyhow::Result<()> {
    let mut list = trusted()?;
    list.retain(|t| t.id != id);
    store_set(TRUSTED, &serde_json::to_string(&list)?)?;
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
        let refs = vec!["op://kasaterm-agents/db/password".to_string()];
        let text = challenge("geno", "book", nonce, 100_000, &origin(), "/repo", "kasaterm-cli op read op://kasaterm-agents/db/password", &refs);
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
        swapped["challenge"] = json!(s.challenge.replace("db/password", "db/other"));
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
    fn only_refs_in_the_one_allowed_vault_are_asked() {
        let s = Settings { token: String::new(), vault_id: "abcd1234".into(), vault_name: "kasaterm-agents".into(), added: 0 };
        let ok = |r: &[&str]| check_refs(&r.iter().map(|x| x.to_string()).collect::<Vec<_>>(), &s).map_err(|e| e.to_string());
        assert!(ok(&["op://kasaterm-agents/db/password"]).is_ok());
        assert!(ok(&["op://Kasaterm-Agents/db/section/password", "op://abcd1234/api/credential?attribute=otp"]).is_ok());
        assert_eq!(ok(&["op://Private/bank/password"]).unwrap_err(), "vault_not_allowed");
        assert_eq!(ok(&["op://kasaterm-agents/db"]).unwrap_err(), "bad_ref");
        assert_eq!(ok(&["https://kasaterm-agents/db/password"]).unwrap_err(), "bad_ref");
        assert_eq!(ok(&["op://kasaterm-agents//password"]).unwrap_err(), "bad_ref");
        assert_eq!(ok(&[]).unwrap_err(), "bad_ref");
    }

    #[test]
    fn the_challenge_masks_other_secrets_in_the_command_but_keeps_the_refs() {
        let refs = vec!["op://kasaterm-agents/db/password".to_string()];
        let text = challenge("geno", "book", "n", 9, &origin(), "/repo",
            "kasaterm-cli op run -e DB=op://kasaterm-agents/db/password -- curl -H 'Authorization: Bearer sk-live-abcdefghijklmnop'", &refs);
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(!v["command"].as_str().unwrap().contains("sk-live"), "{}", v["command"]);
        assert_eq!(v["refs"][0], "op://kasaterm-agents/db/password");
        assert_eq!(v["student"], "유우카");
        assert_eq!(v["kind"], "op.read");
    }

    /// 손으로 돌리는 시험 — 서명 신원이 있는 기계에서 `KASA_OP_SIGTEST_DIR` 에 서명별 kasa-op 사본을 두고
    /// `cargo test -p kasa-mcp op_secret::tests::helper_signature -- --ignored` (docs/op-faceid-approval.md).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn helper_signature_must_be_our_team_and_name() {
        let dir = PathBuf::from(std::env::var("KASA_OP_SIGTEST_DIR").expect("KASA_OP_SIGTEST_DIR"));
        let team = std::env::var("KASA_OP_SIGTEST_TEAM").expect("KASA_OP_SIGTEST_TEAM");
        let check = |name: &str| {
            let mut child = std::process::Command::new(dir.join(name))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let verdict = verify_helper_team(child.id(), &team).is_ok();
            let _ = child.kill();
            let _ = child.wait();
            verdict
        };
        assert!(check("ours/kasa-op"), "우리 팀이 kasa-op 이름으로 서명한 실행기를 거절했다");
        assert!(!check("adhoc/kasa-op"), "서명 없는(ad-hoc) 실행기를 받았다");
        assert!(!check("renamed/kasa-op"), "다른 이름으로 서명한 실행기를 받았다");
        assert!(!check("other-team/kasa-op"), "다른 팀이 서명한 실행기를 받았다");
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
