//! `/relay/approvals` — Claude Code 권한 요청을 같은 계정의 폰·다른 맥에서 허락·거절한다
//! (docs/remote-approval.md).
//!
//! - 데스크톱(요청이 난 기기)이 가린 글로 요청을 올린다. 관문은 그 기기 이름을 **자기 기록**에서
//!   붙인다 — 요청 본문이 「어느 기기」를 말하게 두지 않는다.
//! - 요청은 일회용이다. 처음 온 결정 하나만 받고, 2분이 지나면 만료다. 원래 칸은 그때 자기 권한 창으로 간다.
//! - 결정에는 그 화면이 본 글의 지문이 따라온다. 다르면 받지 않는다(보인 것과 다른 것을 허락하지 않게).
//! - 요청은 메모리에만 둔다(2분짜리다). 남는 것은 감사 줄뿐 — 누가·언제·어느 기기에서 무엇을.
//! - 계정 폰이 맡긴 푸시 토큰으로 관문이 직접 알린다. 다른 곳에서 닫히면 같은 알림을 조용히 갈아 끼운다.
//! - 비밀 요청(`kind:"secret"`, docs/op-faceid-approval.md)은 폰만 허락하고, 그 폰이 맡긴 Secure Enclave
//!   키로 데스크톱이 만든 도전값에 서명해야 받는다. 요청한 맥이 같은 검사를 한 번 더 한다.

use super::*;
use crate::approval_text::{self, key_id, signature_ok, Field};
use serde::Deserialize;
use serde_json::{json, Value};

/// 요청 하나가 기다리는 시간의 상한(초). 데스크톱이 더 짧게 달라고 할 수는 있다.
pub(super) const TTL: u64 = 120;
/// 닫힌 요청을 목록에 남겨 두는 시간 — 다른 화면이 「어디서 닫혔나」를 볼 수 있게.
const KEEP_CLOSED_MS: u64 = 120_000;
const MAX_OPEN_PER_ACCOUNT: usize = 32;
const MAX_FIELDS: usize = 32;
const MAX_WAIT: u64 = 25;
const MAX_BODY: usize = 128 * 1024;
const AUDIT_ROTATE: u64 = 2 * 1024 * 1024;
const MAX_PUSH_PER_DEVICE: usize = 1;
const MAX_CHALLENGE: usize = 4096;
const MAX_REFS: usize = 16;

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/approvals", get(list).post(create))
        .route("/relay/approvals/audit", get(audit))
        .route("/relay/approvals/approver", axum::routing::post(approver))
        .route("/relay/approvals/push", axum::routing::post(push_register).delete(push_unregister))
        .route("/relay/approvals/keys", get(keys_list).post(key_register).delete(key_unregister))
        .route("/relay/approvals/{id}", get(one))
        .route("/relay/approvals/{id}/decide", axum::routing::post(decide))
        .route("/relay/approvals/{id}/cancel", axum::routing::post(cancel))
        .layer(axum::middleware::map_response(no_store))
}

async fn no_store(mut response: axum::response::Response) -> axum::response::Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    response
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Pending,
    Allowed,
    Denied,
    Expired,
    Cancelled,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Device {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug)]
struct Approval {
    id: String,
    account: String,
    /// `tool`(학생 권한 요청) 또는 `secret`(1Password 비밀 읽기 — 폰 서명이 있어야 허락).
    kind: &'static str,
    /// 비밀 요청의 도전값 원문. 폰은 이 글자 그대로에 서명하고 요청한 맥은 올린 것과 같은지 본다.
    challenge: Option<String>,
    proof: Option<(String, String)>,
    origin: Device,
    student: String,
    pane: String,
    cwd: String,
    tool: String,
    fields: Vec<Field>,
    truncated: bool,
    created_ms: u64,
    expires_ms: u64,
    digest: String,
    phase: Phase,
    closed_ms: Option<u64>,
    by: Option<Device>,
    reason: Option<String>,
}

impl Approval {
    fn view(&self, now_ms: u64) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "challenge": self.challenge,
            "key": self.proof.as_ref().map(|p| &p.0),
            "sig": self.proof.as_ref().map(|p| &p.1),
            "state": self.phase.name(),
            "machine": self.origin.label,
            "device": self.origin.id,
            "student": self.student,
            "pane": self.pane,
            "cwd": self.cwd,
            "tool": self.tool,
            "fields": self.fields,
            "truncated": self.truncated,
            "created": self.created_ms,
            "expires": self.expires_ms,
            "now": now_ms,
            "digest": self.digest,
            "closed": self.closed_ms,
            "by": self.by.as_ref().map(|d| json!({"device": d.id, "label": d.label, "kind": d.kind})),
            "reason": self.reason,
        })
    }

    /// 한 줄 요약 — 알림·감사 줄. 비밀은 이미 가려져 있다.
    fn preview(&self) -> String {
        let first = self.fields.first().map(|f| f.text.as_str()).unwrap_or("");
        let line: String = first.lines().next().unwrap_or("").chars().take(160).collect();
        line
    }
}

#[allow(clippy::too_many_arguments)]
fn digest_of(id: &str, origin: &Device, student: &str, pane: &str, cwd: &str, tool: &str, expires_ms: u64, truncated: bool, fields: &[Field], challenge: Option<&str>) -> String {
    let expires = expires_ms.to_string();
    let cut = if truncated { "1" } else { "0" };
    let mut parts = vec![id, &origin.id, &origin.label, student, pane, cwd, tool, &expires, cut];
    // 도구 요청의 지문은 예전 그대로 둔다 — 옛 폰·맥이 받은 지문이 바뀌지 않게.
    if let Some(challenge) = challenge {
        parts.push("secret");
        parts.push(challenge);
    }
    approval_text::digest(&parts, fields)
}

/// 데스크톱이 만든 비밀 요청 도전값 — 관문은 서명하지 않고 모양과 묶임(계정·기기·만료)만 본다.
#[derive(Debug)]
pub(super) struct Challenge {
    student: String,
    pane: String,
    cwd: String,
    command: String,
    refs: Vec<String>,
    exp: u64,
}

pub(super) fn parse_challenge(text: &str, account: &str, device: &str, now: u64) -> Result<Challenge, Refuse> {
    if text.is_empty() || text.len() > MAX_CHALLENGE {
        return Err(Refuse::ChallengeInvalid);
    }
    let v: Value = serde_json::from_str(text).map_err(|_| Refuse::ChallengeInvalid)?;
    let s = |k: &str| v[k].as_str().map(str::to_string);
    let nonce = s("nonce").unwrap_or_default();
    let refs: Vec<String> = v["refs"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(str::to_string)).collect();
    let ok = v["v"] == 1
        && v["kind"] == "op.read"
        && nonce.len() == 32
        && nonce.bytes().all(|b| b.is_ascii_hexdigit())
        && s("account").as_deref() == Some(account)
        && s("device").as_deref() == Some(device)
        && !refs.is_empty()
        && refs.len() <= MAX_REFS
        && v["refs"].as_array().is_some_and(|a| a.len() == refs.len())
        && refs.iter().all(|r| r.starts_with("op://") && short(r, 256));
    if !ok {
        return Err(Refuse::ChallengeInvalid);
    }
    let exp = v["exp"].as_u64().ok_or(Refuse::ChallengeInvalid)?;
    if exp <= now || exp > now + TTL * 1000 + 5_000 {
        return Err(Refuse::ChallengeInvalid);
    }
    let field = |k: &str, max: usize| s(k).filter(|t| short(t, max)).ok_or(Refuse::ChallengeInvalid);
    Ok(Challenge {
        student: field("student", 64)?,
        pane: field("pane", 64)?,
        cwd: field("cwd", 1024)?,
        command: s("command").filter(|t| t.len() <= 2048).ok_or(Refuse::ChallengeInvalid)?,
        refs,
        exp,
    })
}


#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refuse {
    BadRequest,
    DesktopOnly,
    TooMany,
    NotFound,
    NotOrigin,
    Closed,
    Expired,
    DigestMismatch,
    Truncated,
    ApproverRequired,
    ChallengeInvalid,
    PhoneOnly,
    SignatureRequired,
    KeyUnknown,
    BadSignature,
}

impl Refuse {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest | Self::Truncated | Self::ChallengeInvalid => StatusCode::BAD_REQUEST,
            Self::DesktopOnly | Self::NotOrigin | Self::ApproverRequired | Self::PhoneOnly => StatusCode::FORBIDDEN,
            Self::SignatureRequired | Self::KeyUnknown | Self::BadSignature => StatusCode::FORBIDDEN,
            Self::TooMany => StatusCode::TOO_MANY_REQUESTS,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Closed | Self::DigestMismatch => StatusCode::CONFLICT,
            Self::Expired => StatusCode::GONE,
        }
    }
    fn code(&self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::DesktopOnly => "desktop_only",
            Self::TooMany => "too_many_pending",
            Self::NotFound => "not_found",
            Self::NotOrigin => "not_origin",
            Self::Closed => "already_closed",
            Self::Expired => "expired",
            Self::DigestMismatch => "digest_mismatch",
            Self::Truncated => "truncated_cannot_allow",
            Self::ApproverRequired => "approver_required",
            Self::ChallengeInvalid => "challenge_invalid",
            Self::PhoneOnly => "phone_only",
            Self::SignatureRequired => "signature_required",
            Self::KeyUnknown => "key_unknown",
            Self::BadSignature => "bad_signature",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ask {
    #[serde(default)]
    student: String,
    #[serde(default)]
    pane: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool: String,
    /// 도구 입력 원본(JSON). 관문은 가려서 칸으로 편 뒤에만 쥔다 — 데스크톱이 이미 가렸어도 한 번 더.
    #[serde(default)]
    input: Value,
    /// `secret` 이면 `challenge` 가 정본이다 — 칸·학생·폴더는 도전값에서 편다.
    #[serde(default)]
    kind: String,
    #[serde(default)]
    challenge: Option<String>,
    /// 데스크톱이 원문을 다 못 실었다(mod 의 `input_truncated`). 잘린 요청은 허락할 수 없다.
    #[serde(default)]
    truncated: bool,
    ttl: Option<u64>,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct KeyReg {
    /// base64(X9.63 비압축 P-256 공개키)
    public: String,
    id: String,
    added: u64,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct PushReg {
    token: String,
    env: String,
    added: u64,
}

pub(super) struct Store {
    open: Mutex<HashMap<String, Vec<Approval>>>,
    rev: tokio::sync::watch::Sender<u64>,
    /// 기기 → 이 관문 실행 동안 그 앱 화면이 쥔 승인 열쇠의 해시. 재시작하면 앱이 다시 맡긴다.
    approvers: Mutex<HashMap<String, String>>,
    /// 기기(폰) → 푸시 토큰. 기기가 폐기되면 보내지 않는다(보낼 때 기기 기록을 다시 본다).
    push: Mutex<HashMap<String, Vec<PushReg>>>,
    push_path: Option<PathBuf>,
    /// 폰 → 그 폰 Secure Enclave 공개키(기기당 하나). 비밀 요청 허락의 서명을 여기 키로 본다.
    keys: Mutex<HashMap<String, KeyReg>>,
    keys_path: Option<PathBuf>,
    audit_path: Option<PathBuf>,
    /// 시간이 닫은 요청 — 폰 알림을 「처리됨」으로 한 번 갈아 끼우려고 창구가 꺼내 간다.
    expired: Mutex<Vec<(String, Value)>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn new_id() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).expect("system random");
    format!("apv_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

fn valid_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("apv_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn short(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

impl Store {
    pub(super) fn open(state_path: Option<&std::path::Path>) -> Self {
        let push_path = state_path.map(|p| p.with_file_name("relay-push.json"));
        let push = push_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let keys_path = state_path.map(|p| p.with_file_name("relay-approval-keys.json"));
        let keys = keys_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            open: Mutex::new(HashMap::new()),
            rev: tokio::sync::watch::channel(0).0,
            approvers: Mutex::new(HashMap::new()),
            push: Mutex::new(push),
            push_path,
            keys: Mutex::new(keys),
            keys_path,
            audit_path: state_path.map(|p| p.with_file_name("relay-approvals-audit.jsonl")),
            expired: Mutex::new(Vec::new()),
        }
    }

    fn bump(&self) {
        self.rev.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn record(&self, line: Value) {
        let Some(path) = &self.audit_path else { return };
        if std::fs::metadata(path).is_ok_and(|meta| meta.len() > AUDIT_ROTATE) {
            let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        if let Ok(mut file) = options.open(path) {
            use std::io::Write as _;
            let _ = writeln!(file, "{line}");
        }
    }

    fn audit_line(&self, a: &Approval, event: &str, by: Option<&Device>, ip: Option<&str>) {
        self.record(json!({
            "at": now_ms(),
            "account": a.account,
            "id": a.id,
            "event": event,
            "kind": a.kind,
            "key": a.proof.as_ref().map(|p| &p.0),
            "tool": a.tool,
            "preview": a.preview(),
            "digest": a.digest,
            "origin": {"device": a.origin.id, "label": a.origin.label},
            "student": a.student,
            "pane": a.pane,
            "by": by.map(|d| json!({"device": d.id, "label": d.label, "kind": d.kind})),
            "ip": ip,
            "reason": a.reason,
        }));
    }

    /// 만료된 것을 닫고 오래 닫힌 것을 걷는다. 바뀐 게 있으면 true.
    fn sweep(&self, account: &str, now: u64) -> bool {
        let mut expired = Vec::new();
        let changed = {
            let mut open = self.open.lock().unwrap();
            let Some(list) = open.get_mut(account) else { return false };
            let before = list.len();
            for a in list.iter_mut() {
                if a.phase == Phase::Pending && now >= a.expires_ms {
                    a.phase = Phase::Expired;
                    a.closed_ms = Some(now);
                    expired.push(a.clone());
                }
            }
            list.retain(|a| a.closed_ms.is_none_or(|at| now.saturating_sub(at) < KEEP_CLOSED_MS));
            let removed = list.len() != before;
            if list.is_empty() {
                open.remove(account);
            }
            removed || !expired.is_empty()
        };
        for a in &expired {
            self.audit_line(a, "expired", None, None);
        }
        if !expired.is_empty() {
            let mut queue = self.expired.lock().unwrap();
            queue.extend(expired.iter().map(|a| (a.account.clone(), a.view(now))));
        }
        if changed {
            self.bump();
        }
        changed
    }

    pub(super) fn create(&self, account: &str, origin: &Device, ask: Ask, now: u64) -> Result<Value, Refuse> {
        if origin.kind != "desktop" {
            return Err(Refuse::DesktopOnly);
        }
        let mut ask = ask;
        let mut kind = "tool";
        let mut challenge = None;
        let mut deadline = None;
        if ask.kind == "secret" {
            let text = ask.challenge.take().ok_or(Refuse::ChallengeInvalid)?;
            let c = parse_challenge(&text, account, &origin.id, now)?;
            ask.student = c.student;
            ask.pane = c.pane;
            ask.cwd = c.cwd;
            ask.tool = "1Password".into();
            ask.input = json!({"refs": c.refs.join("\n"), "command": c.command});
            ask.truncated = false;
            kind = "secret";
            deadline = Some(c.exp);
            challenge = Some(text);
        } else if !ask.kind.is_empty() && ask.kind != "tool" {
            return Err(Refuse::BadRequest);
        }
        if ask.tool.is_empty()
            || !short(&ask.tool, 64)
            || !short(&ask.student, 64)
            || !short(&ask.pane, 64)
            || !short(&ask.cwd, 1024)
        {
            return Err(Refuse::BadRequest);
        }
        let (fields, cut) = approval_text::fields(&ask.tool, &ask.input);
        let truncated = ask.truncated || cut;
        if fields.len() > MAX_FIELDS || fields.iter().any(|f| !short(&f.label, 64) || !short(&f.name, 64)) {
            return Err(Refuse::BadRequest);
        }
        self.sweep(account, now);
        if kind == "secret" && truncated {
            return Err(Refuse::ChallengeInvalid);
        }
        let ttl = ask.ttl.unwrap_or(TTL).clamp(5, TTL);
        let id = new_id();
        let expires_ms = deadline.map_or(now + ttl * 1000, |d| d.min(now + ttl * 1000));
        let digest = digest_of(&id, origin, &ask.student, &ask.pane, &ask.cwd, &ask.tool, expires_ms, truncated, &fields, challenge.as_deref());
        let a = Approval {
            id,
            account: account.to_string(),
            kind,
            challenge,
            proof: None,
            origin: origin.clone(),
            student: ask.student,
            pane: ask.pane,
            cwd: ask.cwd,
            tool: ask.tool,
            fields,
            truncated,
            created_ms: now,
            expires_ms,
            digest,
            phase: Phase::Pending,
            closed_ms: None,
            by: None,
            reason: None,
        };
        {
            let mut open = self.open.lock().unwrap();
            let list = open.entry(account.to_string()).or_default();
            if list.iter().filter(|a| a.phase == Phase::Pending).count() >= MAX_OPEN_PER_ACCOUNT {
                return Err(Refuse::TooMany);
            }
            list.push(a.clone());
        }
        self.audit_line(&a, "requested", Some(origin), None);
        self.bump();
        Ok(a.view(now))
    }

    pub(super) fn list(&self, account: &str, now: u64) -> Vec<Value> {
        self.sweep(account, now);
        let open = self.open.lock().unwrap();
        open.get(account).map(|l| l.iter().map(|a| a.view(now)).collect()).unwrap_or_default()
    }

    fn find(&self, account: &str, id: &str, now: u64) -> Option<Approval> {
        self.sweep(account, now);
        let open = self.open.lock().unwrap();
        open.get(account)?.iter().find(|a| a.id == id).cloned()
    }

    fn drain_expired(&self) -> Vec<(String, Value)> {
        std::mem::take(&mut *self.expired.lock().unwrap())
    }

    /// 가장 이른 만료 — 기다리는 요청이 그때 깨어 만료를 알린다.
    fn next_deadline(&self, account: &str) -> Option<u64> {
        let open = self.open.lock().unwrap();
        open.get(account)?.iter().filter(|a| a.phase == Phase::Pending).map(|a| a.expires_ms).min()
    }

    pub(super) fn register_approver(&self, device: &str, key: &str) -> Result<(), Refuse> {
        if !opaque(key) {
            return Err(Refuse::BadRequest);
        }
        self.approvers.lock().unwrap().insert(device.into(), crate::relay_auth::token_hash(key));
        Ok(())
    }

    fn approver_ok(&self, device: &str, key: Option<&str>) -> bool {
        let Some(key) = key else { return false };
        self.approvers
            .lock()
            .unwrap()
            .get(device)
            .is_some_and(|hash| *hash == crate::relay_auth::token_hash(key))
    }

    pub(super) fn decide(
        &self,
        account: &str,
        who: &Device,
        approver: Option<&str>,
        id: &str,
        allow: bool,
        digest: &str,
        proof: Option<(&str, &str)>,
        ip: &str,
        now: u64,
    ) -> Result<Value, (Refuse, Option<Value>)> {
        if !self.approver_ok(&who.id, approver) {
            return Err((Refuse::ApproverRequired, None));
        }
        self.sweep(account, now);
        let decided = {
            let mut open = self.open.lock().unwrap();
            let a = open
                .get_mut(account)
                .and_then(|l| l.iter_mut().find(|a| a.id == id))
                .ok_or((Refuse::NotFound, None))?;
            match a.phase {
                Phase::Pending => {}
                Phase::Expired => return Err((Refuse::Expired, Some(a.view(now)))),
                _ => return Err((Refuse::Closed, Some(a.view(now)))),
            }
            if a.digest != digest {
                return Err((Refuse::DigestMismatch, Some(a.view(now))));
            }
            if allow && a.truncated {
                return Err((Refuse::Truncated, Some(a.view(now))));
            }
            if allow && a.kind == "secret" {
                let checked = self.check_proof(who, a.challenge.as_deref().unwrap_or_default(), proof);
                match checked {
                    Ok(key) => a.proof = Some(key),
                    Err(r) => return Err((r, Some(a.view(now)))),
                }
            }
            a.phase = if allow { Phase::Allowed } else { Phase::Denied };
            a.closed_ms = Some(now);
            a.by = Some(who.clone());
            a.clone()
        };
        self.audit_line(&decided, decided.phase.name(), Some(who), Some(ip));
        self.bump();
        Ok(decided.view(now))
    }

    /// 비밀 요청 허락의 증거 — 결정한 기기가 폰이고, 그 폰이 맡긴 키이고, 서명이 도전값에 맞아야 한다.
    fn check_proof(&self, who: &Device, challenge: &str, proof: Option<(&str, &str)>) -> Result<(String, String), Refuse> {
        use base64::Engine as _;
        if who.kind != "phone" {
            return Err(Refuse::PhoneOnly);
        }
        let (key, sig) = proof.ok_or(Refuse::SignatureRequired)?;
        let reg = self.keys.lock().unwrap().get(&who.id).cloned().ok_or(Refuse::KeyUnknown)?;
        if reg.id != key {
            return Err(Refuse::KeyUnknown);
        }
        let engine = base64::engine::general_purpose::STANDARD;
        let public = engine.decode(&reg.public).map_err(|_| Refuse::KeyUnknown)?;
        let raw = engine.decode(sig).map_err(|_| Refuse::BadSignature)?;
        if !signature_ok(&public, challenge, &raw) {
            return Err(Refuse::BadSignature);
        }
        Ok((key.to_string(), sig.to_string()))
    }

    fn save_keys(&self) {
        let Some(path) = &self.keys_path else { return };
        let body = serde_json::to_string_pretty(&*self.keys.lock().unwrap()).unwrap_or_default();
        if let Err(error) = crate::relay_auth::write_private(path, &body) {
            eprintln!("[gateway] approval keys not saved: {error}");
        }
    }

    /// 폰이 Secure Enclave 공개키를 맡긴다. 기기당 하나 — 새로 만들면 갈아 끼운다(옛 서명은 더 안 맞는다).
    pub(super) fn key_register(&self, account: &str, device: &Device, public: &str) -> Result<Value, Refuse> {
        use base64::Engine as _;
        if device.kind != "phone" {
            return Err(Refuse::PhoneOnly);
        }
        let raw = base64::engine::general_purpose::STANDARD.decode(public).map_err(|_| Refuse::BadRequest)?;
        if raw.len() != 65 || raw[0] != 4 {
            return Err(Refuse::BadRequest);
        }
        let id = key_id(&raw);
        self.keys.lock().unwrap().insert(
            device.id.clone(),
            KeyReg { public: public.to_string(), id: id.clone(), added: crate::relay_auth::now_secs() },
        );
        self.save_keys();
        self.record(json!({"at": now_ms(), "account": account, "event": "key_registered", "device": device.id, "key": id}));
        Ok(json!({"id": id}))
    }

    pub(super) fn key_unregister(&self, account: &str, device: &Device) {
        let removed = self.keys.lock().unwrap().remove(&device.id);
        if let Some(reg) = removed {
            self.save_keys();
            self.record(json!({"at": now_ms(), "account": account, "event": "key_removed", "device": device.id, "key": reg.id}));
        }
    }

    /// 주어진 (살아 있는) 폰들의 키.
    fn keys_for(&self, phones: &[(String, String)]) -> Vec<Value> {
        let keys = self.keys.lock().unwrap();
        phones
            .iter()
            .filter_map(|(id, label)| {
                let k = keys.get(id)?;
                Some(json!({"device": id, "label": label, "id": k.id, "public": k.public, "added": k.added}))
            })
            .collect()
    }

    /// 요청한 기기만 — 원래 칸에서 답했거나(로컬 권한 창) 그 칸이 사라졌을 때.
    pub(super) fn cancel(&self, account: &str, who: &Device, id: &str, reason: &str, now: u64) -> Result<Value, (Refuse, Option<Value>)> {
        let reason = match reason {
            "local" | "gone" | "timeout" => reason,
            _ => return Err((Refuse::BadRequest, None)),
        };
        self.sweep(account, now);
        let closed = {
            let mut open = self.open.lock().unwrap();
            let a = open
                .get_mut(account)
                .and_then(|l| l.iter_mut().find(|a| a.id == id))
                .ok_or((Refuse::NotFound, None))?;
            if a.origin.id != who.id {
                return Err((Refuse::NotOrigin, None));
            }
            if a.phase != Phase::Pending {
                return Err((Refuse::Closed, Some(a.view(now))));
            }
            a.phase = Phase::Cancelled;
            a.closed_ms = Some(now);
            a.reason = Some(reason.to_string());
            a.clone()
        };
        self.audit_line(&closed, "cancelled", Some(who), None);
        self.bump();
        Ok(closed.view(now))
    }

    pub(super) fn audit(&self, account: &str, limit: usize) -> Vec<Value> {
        let Some(path) = &self.audit_path else { return Vec::new() };
        let mut lines: Vec<Value> = [path.with_extension("jsonl.1"), path.clone()]
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .flat_map(|text| text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).collect::<Vec<_>>())
            .filter(|line| line["account"] == account)
            .collect();
        lines.reverse();
        lines.truncate(limit);
        lines
    }

    fn save_push(&self) {
        let Some(path) = &self.push_path else { return };
        let body = serde_json::to_string_pretty(&*self.push.lock().unwrap()).unwrap_or_default();
        if let Err(error) = crate::relay_auth::write_private(path, &body) {
            eprintln!("[gateway] push registrations not saved: {error}");
        }
    }

    fn push_register(&self, device: &str, token: &str, env: &str) -> Result<(), Refuse> {
        if token.is_empty() || token.len() > 200 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Refuse::BadRequest);
        }
        let env = if env == "dev" { "dev" } else { "prod" };
        {
            let mut push = self.push.lock().unwrap();
            // 같은 토큰이 다른 기기 아래 있으면 그쪽에서 걷는다 — 폰 하나가 두 번 받지 않게.
            for regs in push.values_mut() {
                regs.retain(|r| r.token != token);
            }
            push.retain(|_, regs| !regs.is_empty());
            let regs = push.entry(device.to_string()).or_default();
            regs.push(PushReg { token: token.into(), env: env.into(), added: crate::relay_auth::now_secs() });
            let excess = regs.len().saturating_sub(MAX_PUSH_PER_DEVICE);
            regs.drain(..excess);
        }
        self.save_push();
        Ok(())
    }

    fn push_unregister(&self, device: &str) {
        let removed = self.push.lock().unwrap().remove(device).is_some();
        if removed {
            self.save_push();
        }
    }

    fn drop_token(&self, token: &str) {
        {
            let mut push = self.push.lock().unwrap();
            for regs in push.values_mut() {
                regs.retain(|r| r.token != token);
            }
            push.retain(|_, regs| !regs.is_empty());
        }
        self.save_push();
    }

    fn set_env(&self, token: &str, env: &str) {
        for regs in self.push.lock().unwrap().values_mut() {
            for r in regs.iter_mut().filter(|r| r.token == token) {
                r.env = env.into();
            }
        }
        self.save_push();
    }

    fn push_targets(&self, devices: &[String]) -> Vec<PushReg> {
        let push = self.push.lock().unwrap();
        devices.iter().filter_map(|d| push.get(d)).flatten().cloned().collect()
    }
}

fn opaque(value: &str) -> bool {
    value.len() == 43 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

// ── 푸시 ──────────────────────────────────────────────────────────────────────

/// 새 요청 알림. 잠금 화면에도 뜨니 비밀은 이미 가린 첫 줄만, 160자까지.
fn push_new(v: &Value) -> Value {
    let student = v["student"].as_str().filter(|s| !s.is_empty()).unwrap_or("학생");
    let machine = v["machine"].as_str().unwrap_or("");
    let tool = v["tool"].as_str().unwrap_or("");
    let first = v["fields"][0]["text"].as_str().unwrap_or("");
    let line: String = first.lines().next().unwrap_or("").chars().take(160).collect();
    let title = if v["kind"] == "secret" { format!("{student} · 1Password 승인 요청") } else { format!("{student} · 승인 요청") };
    json!({
        "aps": {
            "alert": {"title": title, "subtitle": format!("{machine} · {tool}"), "body": line},
            "sound": "default",
            "thread-id": "approval",
            "interruption-level": "time-sensitive",
        },
        "kind": "approval",
        "approval": v["id"],
    })
}

/// 다른 곳에서 닫힌 요청 — 같은 알림 자리를 조용히 갈아 끼운다(소리·배너 없음).
fn push_closed(v: &Value) -> Value {
    let student = v["student"].as_str().filter(|s| !s.is_empty()).unwrap_or("학생");
    let tool = v["tool"].as_str().unwrap_or("");
    let by = v["by"]["label"].as_str().unwrap_or("");
    let what = match v["state"].as_str().unwrap_or("") {
        "allowed" => format!("{by}에서 허락함"),
        "denied" => format!("{by}에서 거절함"),
        "expired" => "시간이 지나 원래 창으로 돌아감".to_string(),
        _ => "원래 창에서 답함".to_string(),
    };
    json!({
        "aps": {
            "alert": {"title": format!("{student} · 처리됨"), "body": format!("{tool} — {what}")},
            "thread-id": "approval",
            "interruption-level": "passive",
        },
        "kind": "approval",
        "approval": v["id"],
        "closed": true,
    })
}

/// 이 계정의 살아 있는 폰들.
fn account_phones(gate: &Gate, account: &str) -> Vec<String> {
    gate.devices
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, d)| d.account == account && d.kind == "phone" && d.revoked_at.is_none())
        .map(|(id, _)| id.clone())
        .collect()
}

/// 이 계정의 살아 있는 폰들과 이름 — 키 목록용.
fn account_phone_labels(gate: &Gate, account: &str) -> Vec<(String, String)> {
    gate.devices
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, d)| d.account == account && d.kind == "phone" && d.revoked_at.is_none())
        .map(|(id, d)| (id.clone(), if d.label.is_empty() { "폰".to_string() } else { d.label.clone() }))
        .collect()
}

/// 닫힘 알림은 결정한 폰에도 간다 — 소리 없이 그 폰 알림 센터의 「승인 요청」을 「처리됨」으로 바꿔 둔다.
/// 닫힘도 우선순위 10 이다 — 5 는 몇 분씩 늦어(가상 아이폰 실측) 이미 닫힌 요청이 「승인 요청」으로 남는다.
pub(super) fn send_push(gate: &Gate, account: &str, payload: Value) {
    let targets = gate.approvals.push_targets(&account_phones(gate, account));
    if targets.is_empty() {
        return;
    }
    let store = gate.approvals.clone();
    let collapse = payload["approval"].as_str().or(payload["collapse"].as_str()).unwrap_or("approval").to_string();
    tokio::spawn(async move {
        for t in targets {
            match crate::push::send_to(&t.env, &t.token, &payload, &collapse).await {
                Ok(()) => {}
                Err((_, text)) if text.contains("BadDeviceToken") => {
                    // 폰이 말한 환경이 틀렸을 수 있다(개발 서명판·시뮬레이터는 샌드박스 토큰) — 반대쪽에 한 번 더.
                    let other = if t.env == "dev" { "prod" } else { "dev" };
                    match crate::push::send_to(other, &t.token, &payload, &collapse).await {
                        Ok(()) => store.set_env(&t.token, other),
                        Err((status, _)) => {
                            eprintln!("[gateway] approval push token dropped ({status}, both environments)");
                            store.drop_token(&t.token);
                        }
                    }
                }
                Err((status, text)) if status == 410 || text.contains("Unregistered") => {
                    eprintln!("[gateway] approval push token dropped ({status})");
                    store.drop_token(&t.token);
                }
                Err((status, text)) => {
                    let reason: String = text.chars().take(120).collect();
                    eprintln!("[gateway] approval push failed ({status}): {reason}");
                }
            }
        }
    });
}

// ── 창구 ──────────────────────────────────────────────────────────────────────

fn who(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(String, Device), axum::response::Response> {
    let Some((id, rec)) = gate.device_of(headers) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    let label = if rec.label.is_empty() { rec.machine_id.clone().unwrap_or_default() } else { rec.label.clone() };
    Ok((rec.account, Device { id, label, kind: rec.kind }))
}

fn refuse(r: Refuse, view: Option<Value>) -> axum::response::Response {
    let mut body = json!({"ok": false, "error": r.code()});
    if let Some(v) = view {
        body["approval"] = v;
    }
    (r.status(), axum::Json(body)).into_response()
}

async fn read<T: serde::de::DeserializeOwned>(req: axum::extract::Request) -> Result<T, axum::response::Response> {
    let bytes = tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), MAX_BODY))
        .await
        .map_err(|_| json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout"))?
        .map_err(|_| json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large"))?;
    serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))
}

async fn create(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (account, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let ask: Ask = match read(req).await {
        Ok(ask) => ask,
        Err(refusal) => return refusal,
    };
    match gate.approvals.create(&account, &device, ask, now_ms()) {
        Ok(view) => {
            send_push(&gate, &account, push_new(&view));
            axum::Json(json!({"ok": true, "approval": view})).into_response()
        }
        Err(r) => refuse(r, None),
    }
}

#[derive(Deserialize)]
struct WaitQuery {
    wait: Option<u64>,
    since: Option<u64>,
}

/// 바뀔 때까지(또는 가장 이른 만료·`wait` 초까지) 붙든다. `since` 가 지금 판과 다르면 바로 답한다.
async fn hold(gate: &Gate, account: &str, query: &WaitQuery, done: impl Fn(&Gate) -> bool) -> u64 {
    let mut rx = gate.approvals.rev.subscribe();
    let wait = query.wait.unwrap_or(0).min(MAX_WAIT);
    let until = tokio::time::Instant::now() + Duration::from_secs(wait);
    loop {
        gate.approvals.sweep(account, now_ms());
        let rev = *rx.borrow_and_update();
        if wait == 0 || query.since.is_some_and(|s| s != rev) || done(gate) {
            return rev;
        }
        let mut wake = until;
        if let Some(deadline) = gate.approvals.next_deadline(account) {
            let left = deadline.saturating_sub(now_ms());
            wake = wake.min(tokio::time::Instant::now() + Duration::from_millis(left + 50));
        }
        if tokio::time::Instant::now() >= until {
            return rev;
        }
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { return rev },
            _ = tokio::time::sleep_until(wake) => {}
        }
        if tokio::time::Instant::now() >= until {
            gate.approvals.sweep(account, now_ms());
            return *rx.borrow();
        }
    }
}

async fn list(State(gate): State<Gate>, headers: axum::http::HeaderMap, axum::extract::Query(query): axum::extract::Query<WaitQuery>) -> axum::response::Response {
    let (account, _) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let rev = hold(&gate, &account, &query, |_| false).await;
    let items = gate.approvals.list(&account, now_ms());
    notify_expired(&gate);
    axum::Json(json!({"ok": true, "rev": rev, "now": now_ms(), "approvals": items})).into_response()
}

async fn one(
    State(gate): State<Gate>,
    AxPath(id): AxPath<String>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(query): axum::extract::Query<WaitQuery>,
) -> axum::response::Response {
    let (account, _) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if !valid_id(&id) {
        return refuse(Refuse::NotFound, None);
    }
    if gate.approvals.find(&account, &id, now_ms()).is_none() {
        return refuse(Refuse::NotFound, None);
    }
    let account_ref = account.clone();
    let id_ref = id.clone();
    hold(&gate, &account, &query, move |g| {
        g.approvals.find(&account_ref, &id_ref, now_ms()).is_none_or(|a| a.phase != Phase::Pending)
    })
    .await;
    notify_expired(&gate);
    match gate.approvals.find(&account, &id, now_ms()) {
        Some(a) => {
            axum::Json(json!({"ok": true, "approval": a.view(now_ms())})).into_response()
        }
        None => refuse(Refuse::NotFound, None),
    }
}

/// 만료는 누가 결정한 게 아니라 시간이 닫는다 — 그 순간을 본 창구가 폰 알림을 한 번 갈아 끼운다.
fn notify_expired(gate: &Gate) {
    for (account, view) in gate.approvals.drain_expired() {
        send_push(gate, &account, push_closed(&view));
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    decision: String,
    digest: String,
    /// 비밀 요청 허락: 폰 키 id 와 base64 DER 서명.
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    sig: Option<String>,
}

async fn decide(State(gate): State<Gate>, AxPath(id): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let (account, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if !valid_id(&id) {
        return refuse(Refuse::NotFound, None);
    }
    let ip = client_ip(&req);
    let approver = req.headers().get("x-kasa-approver").and_then(|v| v.to_str().ok()).map(str::to_string);
    let body: Decision = match read(req).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let allow = match body.decision.as_str() {
        "allow" => true,
        "deny" => false,
        _ => return refuse(Refuse::BadRequest, None),
    };
    let proof = body.key.as_deref().zip(body.sig.as_deref());
    match gate.approvals.decide(&account, &device, approver.as_deref(), &id, allow, &body.digest, proof, &ip, now_ms()) {
        Ok(view) => {
            send_push(&gate, &account, push_closed(&view));
            axum::Json(json!({"ok": true, "approval": view})).into_response()
        }
        Err((r, view)) => refuse(r, view),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelBody {
    reason: String,
}

async fn cancel(State(gate): State<Gate>, AxPath(id): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let (account, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if !valid_id(&id) {
        return refuse(Refuse::NotFound, None);
    }
    let body: CancelBody = match read(req).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    match gate.approvals.cancel(&account, &device, &id, &body.reason, now_ms()) {
        Ok(view) => {
            send_push(&gate, &account, push_closed(&view));
            axum::Json(json!({"ok": true, "approval": view})).into_response()
        }
        Err((r, view)) => refuse(r, view),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproverBody {
    key: String,
}

async fn approver(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (_, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let body: ApproverBody = match read(req).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    match gate.approvals.register_approver(&device.id, &body.key) {
        Ok(()) => axum::Json(json!({"ok": true})).into_response(),
        Err(r) => refuse(r, None),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PushBody {
    token: String,
    #[serde(default)]
    env: String,
}

async fn push_register(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (_, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if device.kind != "phone" {
        return refuse(Refuse::BadRequest, None);
    }
    let body: PushBody = match read(req).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    match gate.approvals.push_register(&device.id, &body.token, &body.env) {
        Ok(()) => axum::Json(json!({"ok": true, "ready": crate::push::configured()})).into_response(),
        Err(r) => refuse(r, None),
    }
}

async fn push_unregister(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (_, device) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    gate.approvals.push_unregister(&device.id);
    axum::Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyBody {
    public: String,
}

async fn key_register(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (account, device) = match who(&gate, req.headers()) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let body: KeyBody = match read(req).await {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    match gate.approvals.key_register(&account, &device, &body.public) {
        Ok(v) => axum::Json(json!({"ok": true, "key": v})).into_response(),
        Err(r) => refuse(r, None),
    }
}

async fn key_unregister(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (account, device) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    gate.approvals.key_unregister(&account, &device);
    axum::Json(json!({"ok": true})).into_response()
}

/// 계정의 살아 있는 폰 키 — 맥은 여기 있으면서 사람이 맥에서 믿은 키만 쓴다.
async fn keys_list(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (account, _) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let keys = gate.approvals.keys_for(&account_phone_labels(&gate, &account));
    axum::Json(json!({"ok": true, "keys": keys})).into_response()
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

async fn audit(State(gate): State<Gate>, headers: axum::http::HeaderMap, axum::extract::Query(query): axum::extract::Query<AuditQuery>) -> axum::response::Response {
    let (account, _) = match who(&gate, &headers) {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let lines = gate.approvals.audit(&account, query.limit.unwrap_or(50).min(200));
    axum::Json(json!({"ok": true, "audit": lines})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval_text::SECRET_PREFIX;

    fn desk(id: &str) -> Device {
        Device { id: id.into(), label: format!("{id} 맥"), kind: "desktop".into() }
    }
    fn phone(id: &str) -> Device {
        Device { id: id.into(), label: "폰".into(), kind: "phone".into() }
    }
    fn ask(cmd: &str) -> Ask {
        Ask {
            student: "유우카".into(),
            pane: "%3".into(),
            cwd: "/repo".into(),
            tool: "Bash".into(),
            input: json!({"command": cmd}),
            truncated: false,
            ttl: None,
            kind: String::new(),
            challenge: None,
        }
    }

    struct Signer(ring::signature::EcdsaKeyPair);

    impl Signer {
        fn new() -> Self {
            let rng = ring::rand::SystemRandom::new();
            let alg = &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING;
            let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(alg, &rng).unwrap();
            Self(ring::signature::EcdsaKeyPair::from_pkcs8(alg, pkcs8.as_ref(), &rng).unwrap())
        }
        fn public(&self) -> String {
            use base64::Engine as _;
            use ring::signature::KeyPair as _;
            base64::engine::general_purpose::STANDARD.encode(self.0.public_key().as_ref())
        }
        fn id(&self) -> String {
            use ring::signature::KeyPair as _;
            key_id(self.0.public_key().as_ref())
        }
        fn sign(&self, challenge: &str) -> String {
            use base64::Engine as _;
            let mut message = SECRET_PREFIX.to_vec();
            message.extend_from_slice(challenge.as_bytes());
            let sig = self.0.sign(&ring::rand::SystemRandom::new(), &message).unwrap();
            base64::engine::general_purpose::STANDARD.encode(sig.as_ref())
        }
    }

    fn challenge(device: &str, now: u64, nonce: &str) -> String {
        json!({"v": 1, "kind": "op.read", "nonce": nonce, "account": "geno", "device": device,
            "student": "유우카", "pane": "%3", "cwd": "/repo",
            "command": "kasaterm-cli op read op://kasaterm-agents/db/password",
            "refs": ["op://kasaterm-agents/db/password"], "exp": now + 60_000})
        .to_string()
    }

    fn secret_ask(challenge: String) -> Ask {
        Ask { kind: "secret".into(), challenge: Some(challenge), tool: String::new(), input: Value::Null, ..ask("") }
    }

    const NONCE: &str = "00112233445566778899aabbccddeeff";
    const KEY: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

    fn store() -> (Store, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        (Store::open(Some(&dir.path().join("relay-state.json"))), dir)
    }

    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("apv-test-{}", super::new_id()));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn only_desktops_ask_and_the_request_is_masked_and_named_by_the_gateway() {
        let (s, _d) = store();
        assert_eq!(s.create("geno", &phone("p1"), ask("ls"), 1_000).unwrap_err(), Refuse::DesktopOnly);
        let v = s.create("geno", &desk("book"), ask("API_KEY=hunter2 cargo run"), 1_000).unwrap();
        assert_eq!(v["machine"], "book 맥");
        assert_eq!(v["state"], "pending");
        assert_eq!(v["expires"], 1_000 + TTL * 1000);
        let text = v["fields"][0]["text"].as_str().unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.ends_with("cargo run"));
        assert!(s.list("other", 1_000).is_empty(), "다른 계정에 보였다");
    }

    #[test]
    fn the_first_decision_wins_and_later_ones_see_who_closed_it() {
        let (s, _d) = store();
        let v = s.create("geno", &desk("book"), ask("rm -rf build"), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        let digest = v["digest"].as_str().unwrap();
        s.register_approver("p1", KEY).unwrap();
        s.register_approver("mini", KEY).unwrap();
        assert_eq!(s.decide("geno", &phone("p1"), None, id, true, digest, None, "ip", 2_000).unwrap_err().0, Refuse::ApproverRequired);
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, true, "nope", None, "ip", 2_000).unwrap_err().0, Refuse::DigestMismatch);
        let done = s.decide("geno", &phone("p1"), Some(KEY), id, true, digest, None, "ip", 2_000).unwrap();
        assert_eq!(done["state"], "allowed");
        assert_eq!(done["by"]["kind"], "phone");
        let (r, view) = s.decide("geno", &desk("mini"), Some(KEY), id, false, digest, None, "ip", 2_100).unwrap_err();
        assert_eq!(r, Refuse::Closed);
        assert_eq!(view.unwrap()["by"]["device"], "p1");
        assert_eq!(s.decide("other", &phone("p1"), Some(KEY), id, true, digest, None, "ip", 2_000).unwrap_err().0, Refuse::NotFound);
        let audit = s.audit("geno", 10);
        assert_eq!(audit[0]["event"], "allowed");
        assert_eq!(audit[0]["by"]["device"], "p1");
        assert_eq!(audit[0]["ip"], "ip");
        assert_eq!(audit[1]["event"], "requested");
    }

    #[test]
    fn requests_expire_after_two_minutes_and_cannot_be_decided() {
        let (s, _d) = store();
        let v = s.create("geno", &desk("book"), ask("ls"), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        s.register_approver("p1", KEY).unwrap();
        let late = 1_000 + TTL * 1000;
        let (r, view) = s.decide("geno", &phone("p1"), Some(KEY), id, true, v["digest"].as_str().unwrap(), None, "ip", late).unwrap_err();
        assert_eq!(r, Refuse::Expired);
        assert_eq!(view.unwrap()["state"], "expired");
        assert_eq!(s.audit("geno", 1)[0]["event"], "expired");
        assert!(s.list("geno", late + KEEP_CLOSED_MS).is_empty(), "닫힌 요청이 안 걷혔다");
    }

    #[test]
    fn only_the_origin_cancels_and_truncated_requests_cannot_be_allowed() {
        let (s, _d) = store();
        let v = s.create("geno", &desk("book"), ask("ls"), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        assert_eq!(s.cancel("geno", &desk("mini"), id, "local", 1_100).unwrap_err().0, Refuse::NotOrigin);
        let c = s.cancel("geno", &desk("book"), id, "local", 1_100).unwrap();
        assert_eq!(c["state"], "cancelled");
        assert_eq!(c["reason"], "local");

        let mut big = ask("");
        big.tool = "Write".into();
        big.input = json!({"file_path": "/a", "content": "x".repeat(approval_text::MAX_FIELD + 10)});
        let v = s.create("geno", &desk("book"), big, 1_000).unwrap();
        assert_eq!(v["truncated"], true);
        s.register_approver("p1", KEY).unwrap();
        let id = v["id"].as_str().unwrap();
        let digest = v["digest"].as_str().unwrap();
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, true, digest, None, "ip", 1_100).unwrap_err().0, Refuse::Truncated);
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, false, digest, None, "ip", 1_100).unwrap()["state"], "denied");
    }

    #[test]
    fn a_phone_token_belongs_to_one_device_and_survives_restart() {
        let (s, d) = store();
        s.push_register("p1", "abcdef", "dev").unwrap();
        s.push_register("p2", "abcdef", "prod").unwrap();
        assert!(s.push_targets(&["p1".into()]).is_empty(), "옮겨 간 토큰이 옛 기기에 남았다");
        let again = Store::open(Some(&d.path().join("relay-state.json")));
        assert_eq!(again.push_targets(&["p2".into()])[0].env, "prod");
        assert_eq!(s.push_register("p1", "not-hex", "dev").unwrap_err(), Refuse::BadRequest);
        let payload = push_new(&json!({"id":"apv_1","student":"유우카","machine":"맥북","tool":"Bash","fields":[{"text":"ls\nsecond"}]}));
        assert_eq!(payload["aps"]["alert"]["body"], "ls");
        assert_eq!(payload["kind"], "approval");
        let closed = push_closed(&json!({"id":"apv_1","student":"유우카","tool":"Bash","state":"allowed","by":{"label":"맥미니"}}));
        assert_eq!(closed["aps"]["interruption-level"], "passive");
        assert!(closed["aps"].get("sound").is_none());
    }

    #[test]
    fn a_secret_request_is_built_from_its_challenge_and_bound_to_account_and_device() {
        let (s, _d) = store();
        let c = challenge("book", 1_000, NONCE);
        let v = s.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        assert_eq!(v["kind"], "secret");
        assert_eq!(v["challenge"], c);
        assert_eq!(v["tool"], "1Password");
        assert_eq!(v["student"], "유우카");
        assert_eq!(v["expires"], 61_000, "도전값 만료가 더 이르면 그것이 만료다");
        assert_eq!(v["fields"][0]["text"], "op://kasaterm-agents/db/password");
        // 다른 기기·다른 계정을 말하는 도전값, 모양이 틀린 도전값은 받지 않는다.
        let other = challenge("mini", 1_000, NONCE);
        assert_eq!(s.create("geno", &desk("book"), secret_ask(other), 1_000).unwrap_err(), Refuse::ChallengeInvalid);
        assert_eq!(s.create("else", &desk("book"), secret_ask(c.clone()), 1_000).unwrap_err(), Refuse::ChallengeInvalid);
        let stale = challenge("book", 1_000, NONCE);
        assert_eq!(s.create("geno", &desk("book"), secret_ask(stale), 70_000).unwrap_err(), Refuse::ChallengeInvalid);
        let bad = c.replace("op://kasaterm-agents/db/password\"]", "https://x\"]");
        assert_eq!(s.create("geno", &desk("book"), secret_ask(bad), 1_000).unwrap_err(), Refuse::ChallengeInvalid);
        assert_eq!(s.create("geno", &desk("book"), secret_ask(c.replace(NONCE, "short")), 1_000).unwrap_err(), Refuse::ChallengeInvalid);
    }

    #[test]
    fn only_a_registered_phone_key_signing_the_exact_challenge_can_allow_a_secret() {
        let (s, _d) = store();
        let phone_key = Signer::new();
        s.register_approver("p1", KEY).unwrap();
        s.register_approver("p2", KEY).unwrap();
        s.register_approver("mini", KEY).unwrap();
        assert_eq!(s.key_register("geno", &desk("mini"), &phone_key.public()).unwrap_err(), Refuse::PhoneOnly);
        assert_eq!(s.key_register("geno", &phone("p1"), "AAAA").unwrap_err(), Refuse::BadRequest);
        let reg = s.key_register("geno", &phone("p1"), &phone_key.public()).unwrap();
        assert_eq!(reg["id"], phone_key.id());

        let c = challenge("book", 1_000, NONCE);
        let v = s.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        let digest = v["digest"].as_str().unwrap();
        let good = phone_key.sign(&c);
        let kid = phone_key.id();
        let decide = |who: &Device, proof: Option<(&str, &str)>| s.decide("geno", who, Some(KEY), id, true, digest, proof, "ip", 2_000);

        assert_eq!(decide(&desk("mini"), Some((&kid, &good))).unwrap_err().0, Refuse::PhoneOnly);
        assert_eq!(decide(&phone("p1"), None).unwrap_err().0, Refuse::SignatureRequired);
        // 다른 폰은 자기 키가 없다 — p1 의 서명을 실어 와도 안 된다.
        assert_eq!(decide(&phone("p2"), Some((&kid, &good))).unwrap_err().0, Refuse::KeyUnknown);
        // 변조: 도전값 한 글자만 바꿔 서명한 것.
        let tampered = phone_key.sign(&c.replace("password", "passwore"));
        assert_eq!(decide(&phone("p1"), Some((&kid, &tampered))).unwrap_err().0, Refuse::BadSignature);
        let stranger = Signer::new().sign(&c);
        assert_eq!(decide(&phone("p1"), Some((&kid, &stranger))).unwrap_err().0, Refuse::BadSignature);
        assert_eq!(decide(&phone("p1"), Some((&kid, "!!"))).unwrap_err().0, Refuse::BadSignature);

        let done = decide(&phone("p1"), Some((&kid, &good))).unwrap();
        assert_eq!(done["state"], "allowed");
        assert_eq!(done["key"], kid);
        assert_eq!(done["sig"], good);
        // 같은 서명으로 한 번 더(재사용) — 이미 닫혔다.
        assert_eq!(decide(&phone("p1"), Some((&kid, &good))).unwrap_err().0, Refuse::Closed);
        assert_eq!(s.audit("geno", 1)[0]["key"], kid);

        // 거절은 서명 없이 된다 — 거절은 아무것도 열지 않는다.
        let c2 = challenge("book", 1_000, "ffeeddccbbaa99887766554433221100");
        let v2 = s.create("geno", &desk("book"), secret_ask(c2), 1_000).unwrap();
        let denied = s.decide("geno", &phone("p2"), Some(KEY), v2["id"].as_str().unwrap(), false, v2["digest"].as_str().unwrap(), None, "ip", 2_000);
        assert_eq!(denied.unwrap()["state"], "denied");
    }

    #[test]
    fn a_secret_cannot_be_allowed_after_expiry_cancel_or_key_replacement() {
        let (s, d) = store();
        let old = Signer::new();
        s.register_approver("p1", KEY).unwrap();
        s.key_register("geno", &phone("p1"), &old.public()).unwrap();
        let c = challenge("book", 1_000, NONCE);
        let v = s.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        let (id, digest) = (v["id"].as_str().unwrap().to_string(), v["digest"].as_str().unwrap().to_string());
        let sig = old.sign(&c);
        let (r, _) = s.decide("geno", &phone("p1"), Some(KEY), &id, true, &digest, Some((&old.id(), &sig)), "ip", 61_000).unwrap_err();
        assert_eq!(r, Refuse::Expired);

        let c = challenge("book", 1_000, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let v = s.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        s.cancel("geno", &desk("book"), id, "gone", 1_500).unwrap();
        let (r, _) = s.decide("geno", &phone("p1"), Some(KEY), id, true, v["digest"].as_str().unwrap(), Some((&old.id(), &old.sign(&c))), "ip", 2_000).unwrap_err();
        assert_eq!(r, Refuse::Closed);

        // 새 키로 갈아 끼우면 옛 키 서명은 더 안 맞는다. 다시 켜도 키는 남는다.
        let new = Signer::new();
        s.key_register("geno", &phone("p1"), &new.public()).unwrap();
        let again = Store::open(Some(&d.path().join("relay-state.json")));
        again.register_approver("p1", KEY).unwrap();
        let c = challenge("book", 1_000, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let v = again.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        let id = v["id"].as_str().unwrap();
        let digest = v["digest"].as_str().unwrap();
        let (r, _) = again.decide("geno", &phone("p1"), Some(KEY), id, true, digest, Some((&old.id(), &old.sign(&c))), "ip", 2_000).unwrap_err();
        assert_eq!(r, Refuse::KeyUnknown);
        assert_eq!(again.decide("geno", &phone("p1"), Some(KEY), id, true, digest, Some((&new.id(), &new.sign(&c))), "ip", 2_000).unwrap()["state"], "allowed");

        // 폰이 키를 지우면(또는 기기가 폐기되어 목록에서 빠지면) 더는 허락이 안 된다.
        again.key_unregister("geno", &phone("p1"));
        assert!(again.keys_for(&[("p1".into(), "폰".into())]).is_empty());
        let c = challenge("book", 1_000, "cccccccccccccccccccccccccccccccc");
        let v = again.create("geno", &desk("book"), secret_ask(c.clone()), 1_000).unwrap();
        let (r, _) = again.decide("geno", &phone("p1"), Some(KEY), v["id"].as_str().unwrap(), true, v["digest"].as_str().unwrap(), Some((&new.id(), &new.sign(&c))), "ip", 2_000).unwrap_err();
        assert_eq!(r, Refuse::KeyUnknown);
    }

    #[test]
    fn tool_request_digests_are_unchanged_for_old_clients() {
        let origin = desk("book");
        let fields = vec![Field { name: "command".into(), label: "명령".into(), text: "ls".into() }];
        let now = digest_of("apv_1", &origin, "s", "%1", "/", "Bash", 9, false, &fields, None);
        let old = approval_text::digest(&["apv_1", "book", "book 맥", "s", "%1", "/", "Bash", "9", "0"], &fields);
        assert_eq!(now, old);
    }
}
