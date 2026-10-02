//! `/relay/approvals` — Claude Code 권한 요청을 같은 계정의 폰·다른 맥에서 허락·거절한다
//! (docs/remote-approval.md).
//!
//! - 데스크톱(요청이 난 기기)이 가린 글로 요청을 올린다. 관문은 그 기기 이름을 **자기 기록**에서
//!   붙인다 — 요청 본문이 「어느 기기」를 말하게 두지 않는다.
//! - 요청은 일회용이다. 처음 온 결정 하나만 받고, 2분이 지나면 만료다. 원래 칸은 그때 자기 권한 창으로 간다.
//! - 결정에는 그 화면이 본 글의 지문이 따라온다. 다르면 받지 않는다(보인 것과 다른 것을 허락하지 않게).
//! - 요청은 메모리에만 둔다(2분짜리다). 남는 것은 감사 줄뿐 — 누가·언제·어느 기기에서 무엇을.
//! - 계정 폰이 맡긴 푸시 토큰으로 관문이 직접 알린다. 다른 곳에서 닫히면 같은 알림을 조용히 갈아 끼운다.

use super::*;
use crate::approval_text::{self, Field};
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

pub(super) fn routes() -> Router<Gate> {
    Router::new()
        .route("/relay/approvals", get(list).post(create))
        .route("/relay/approvals/audit", get(audit))
        .route("/relay/approvals/approver", axum::routing::post(approver))
        .route("/relay/approvals/push", axum::routing::post(push_register).delete(push_unregister))
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

fn digest_of(id: &str, origin: &Device, student: &str, pane: &str, cwd: &str, tool: &str, expires_ms: u64, truncated: bool, fields: &[Field]) -> String {
    let expires = expires_ms.to_string();
    let cut = if truncated { "1" } else { "0" };
    approval_text::digest(&[id, &origin.id, &origin.label, student, pane, cwd, tool, &expires, cut], fields)
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
}

impl Refuse {
    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest | Self::Truncated => StatusCode::BAD_REQUEST,
            Self::DesktopOnly | Self::NotOrigin | Self::ApproverRequired => StatusCode::FORBIDDEN,
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
    tool: String,
    /// 도구 입력 원본(JSON). 관문은 가려서 칸으로 편 뒤에만 쥔다 — 데스크톱이 이미 가렸어도 한 번 더.
    input: Value,
    /// 데스크톱이 원문을 다 못 실었다(mod 의 `input_truncated`). 잘린 요청은 허락할 수 없다.
    #[serde(default)]
    truncated: bool,
    ttl: Option<u64>,
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
        Self {
            open: Mutex::new(HashMap::new()),
            rev: tokio::sync::watch::channel(0).0,
            approvers: Mutex::new(HashMap::new()),
            push: Mutex::new(push),
            push_path,
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
        let ttl = ask.ttl.unwrap_or(TTL).clamp(5, TTL);
        let id = new_id();
        let expires_ms = now + ttl * 1000;
        let digest = digest_of(&id, origin, &ask.student, &ask.pane, &ask.cwd, &ask.tool, expires_ms, truncated, &fields);
        let a = Approval {
            id,
            account: account.to_string(),
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
            a.phase = if allow { Phase::Allowed } else { Phase::Denied };
            a.closed_ms = Some(now);
            a.by = Some(who.clone());
            a.clone()
        };
        self.audit_line(&decided, decided.phase.name(), Some(who), Some(ip));
        self.bump();
        Ok(decided.view(now))
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
    json!({
        "aps": {
            "alert": {"title": format!("{student} · 승인 요청"), "subtitle": format!("{machine} · {tool}"), "body": line},
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

/// 닫힘 알림은 결정한 폰에도 간다 — 소리 없이 그 폰 알림 센터의 「승인 요청」을 「처리됨」으로 바꿔 둔다.
/// 닫힘도 우선순위 10 이다 — 5 는 몇 분씩 늦어(가상 아이폰 실측) 이미 닫힌 요청이 「승인 요청」으로 남는다.
fn send_push(gate: &Gate, account: &str, payload: Value) {
    let targets = gate.approvals.push_targets(&account_phones(gate, account));
    if targets.is_empty() {
        return;
    }
    let store = gate.approvals.clone();
    let collapse = payload["approval"].as_str().unwrap_or("approval").to_string();
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
    match gate.approvals.decide(&account, &device, approver.as_deref(), &id, allow, &body.digest, &ip, now_ms()) {
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
        }
    }
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
        assert_eq!(s.decide("geno", &phone("p1"), None, id, true, digest, "ip", 2_000).unwrap_err().0, Refuse::ApproverRequired);
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, true, "nope", "ip", 2_000).unwrap_err().0, Refuse::DigestMismatch);
        let done = s.decide("geno", &phone("p1"), Some(KEY), id, true, digest, "ip", 2_000).unwrap();
        assert_eq!(done["state"], "allowed");
        assert_eq!(done["by"]["kind"], "phone");
        let (r, view) = s.decide("geno", &desk("mini"), Some(KEY), id, false, digest, "ip", 2_100).unwrap_err();
        assert_eq!(r, Refuse::Closed);
        assert_eq!(view.unwrap()["by"]["device"], "p1");
        assert_eq!(s.decide("other", &phone("p1"), Some(KEY), id, true, digest, "ip", 2_000).unwrap_err().0, Refuse::NotFound);
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
        let (r, view) = s.decide("geno", &phone("p1"), Some(KEY), id, true, v["digest"].as_str().unwrap(), "ip", late).unwrap_err();
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
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, true, digest, "ip", 1_100).unwrap_err().0, Refuse::Truncated);
        assert_eq!(s.decide("geno", &phone("p1"), Some(KEY), id, false, digest, "ip", 1_100).unwrap()["state"], "denied");
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
}
