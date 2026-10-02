//! claude 안에 실린 연결 mod(`collab-hooks/claude-mods/kasaterm-bridge`)가 이 기계 앱에 알리는 자리.
//! 계약은 `docs/claude-mod-bridge.md`.
//!
//! 화면 격자·기록 꼬리·settings 훅 스크립트로 바깥에서 짐작하던 것을 엔진 이벤트로 받는다. 받은 사실은
//! 칸(`%N`)마다 여기 쥐고, 앱(상태 판정·보드·tell·상태줄)이 읽는다. 사실이 정본이 되는 것은 그 칸에 지금
//! 도는 claude 가 hello 를 보낸 바로 그 프로세스일 때뿐이다 — mod 없이 다시 뜬 claude 의 칸에 지난 mod 의
//! 사실이 남아 판정을 쥐면 안 된다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Query, Request as HttpRequest};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

/// 승인 요청 하나를 쥐는 한도. 넘으면 원격 결정을 안 받고 엔진 창만 남는다.
pub const PERMISSION_TTL: Duration = Duration::from_secs(600);
/// 긴 폴링 한 번이 쥐는 시간. mod 는 같은 요청·같은 칸으로 다시 연다.
const HOLD: Duration = Duration::from_secs(25);
/// mod 가 꺼내 가지 않은 tell 은 이만큼 뒤 대기열로 되돌린다 — 꺼내 가지 않았으니 넣지 않은 것이 확실하다.
const OFFER_UNTAKEN: Duration = Duration::from_secs(20);
/// 꺼내 간 tell 의 결과(ack)를 기다리는 한도. 넘으면 넣었는지 모르는 것(uncertain)이다.
const OFFER_UNACKED: Duration = Duration::from_secs(90);
const ACTIVITY_CAP: usize = 200;
const INPUT_CAP: usize = 64 * 1024;
const BODY_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub context_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub context_percent: Option<f64>,
    pub limits: Vec<Limit>,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    pub kind: String,
    pub percent: f64,
    pub resets_at: Option<String>,
}

/// 서브에이전트·백그라운드 셸·모니터·워크플로 하나.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub label: String,
}

/// 한 칸의 mod 가 알린 지금. 시각은 받은 때(앱 시계)다.
#[derive(Clone, Debug)]
pub struct ModState {
    pub session: String,
    pub pid: u32,
    pub version: String,
    pub turn_open: bool,
    pub turn_at: Instant,
    /// 지난 턴이 오류로 끝났으면 그 까닭(`error`·`refusal`). 다음 턴이 지운다.
    pub turn_error: Option<String>,
    pub compacting: Option<Instant>,
    /// 열린 승인 요청(id, 도구). 여럿이면 앞의 것이 먼저 열린 것.
    pub permissions: Vec<(String, String)>,
    /// 승인 요청이 처음 열린 때 — 그 뒤 칸에 사람 키가 들어오면 창에 답한 것이다(엔진은 그 순간을 안 알린다).
    pub permission_since: Option<Instant>,
    pub question: Option<String>,
    pub usage: Option<Usage>,
    pub background: Vec<Task>,
    pub last_event: Instant,
}

impl ModState {
    fn new(session: &str, pid: u32, version: &str) -> Self {
        let now = Instant::now();
        Self {
            session: session.into(),
            pid,
            version: version.into(),
            turn_open: false,
            turn_at: now,
            turn_error: None,
            compacting: None,
            permissions: Vec::new(),
            permission_since: None,
            question: None,
            usage: None,
            background: Vec::new(),
            last_event: now,
        }
    }

    /// 쉬는가 — tell 을 `$.prompt.submit` 으로 넣어도 되는 순간.
    pub fn resting(&self) -> bool {
        !self.turn_open && self.compacting.is_none() && self.permissions.is_empty() && self.question.is_none()
    }

    pub fn running_tasks(&self) -> impl Iterator<Item = &Task> {
        self.background.iter().filter(|t| matches!(t.status.as_str(), "running" | "pending"))
    }
}

struct Pane {
    state: ModState,
    activity: VecDeque<Value>,
    /// 대화 행이 몇 번 쌓였나 — 대화 보기가 기록 파일을 다시 읽을 때를 안다(내용은 파일이 정본).
    rows: u64,
}

static PANES: LazyLock<Mutex<HashMap<String, Pane>>> = LazyLock::new(Default::default);

type Listener = Arc<dyn Fn(&str) + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();

/// 사실이 바뀐 칸을 앱에 알릴 자리(상태 판정 다시·GUI 깨우기). 앱이 한 번 건다.
pub fn set_listener(listener: impl Fn(&str) + Send + Sync + 'static) {
    let _ = LISTENER.set(Arc::new(listener));
}

fn changed(surface: &str) {
    if let Some(listener) = LISTENER.get() {
        listener(surface);
    }
    crate::board_service::poke();
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// 이벤트 묶음 하나를 적용한다. hello 없이 온 칸의 이벤트는 버린다(앱이 재시작된 뒤 mod 가 다시 인사할 때까지).
pub fn apply(surface: &str, session: &str, events: &[Value]) {
    if surface.is_empty() {
        return;
    }
    let mut touched = false;
    let mut statusline = false;
    {
        let mut panes = PANES.lock().unwrap();
        for event in events {
            let kind = text(event, "kind");
            if kind == "hello" {
                let pid = event.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
                let fresh = panes.get(surface).is_none_or(|p| p.state.session != session || p.state.pid != pid);
                if fresh {
                    panes.insert(
                        surface.to_string(),
                        Pane { state: ModState::new(session, pid, &text(event, "mod")), activity: VecDeque::new(), rows: 0 },
                    );
                }
                touched = true;
                continue;
            }
            if kind == "bye" {
                if panes.get(surface).is_some_and(|p| p.state.session == session) {
                    panes.remove(surface);
                    touched = true;
                }
                continue;
            }
            let Some(pane) = panes.get_mut(surface).filter(|p| p.state.session == session) else { continue };
            pane.state.last_event = Instant::now();
            touched |= apply_one(pane, &kind, event, &mut statusline);
        }
    }
    if statusline {
        write_statusline(surface);
    }
    if touched {
        changed(surface);
    }
}

/// 보드 활동 한 줄 — `kasa_socket::backend::ActivityEvent` 와 같은 칸(`kind`·`name`·`text`·`is_error`).
fn note(pane: &mut Pane, kind: &str, name: &str, body: &str, error: Option<bool>) {
    if body.is_empty() && kind != "tool" {
        return;
    }
    // Enter 때 한 번, 턴 시작 때 또 한 번 같은 프롬프트가 온다.
    if kind == "prompt" && pane.activity.back().is_some_and(|last| last["kind"] == "prompt" && last["text"] == body) {
        return;
    }
    if pane.activity.len() >= ACTIVITY_CAP {
        pane.activity.pop_front();
    }
    pane.activity.push_back(json!({"kind": kind, "name": name, "text": body, "is_error": error}));
}

fn apply_one(pane: &mut Pane, kind: &str, event: &Value, statusline: &mut bool) -> bool {
    match kind {
        "turn" => match text(event, "phase").as_str() {
            "start" => note(pane, "prompt", "", &text(event, "text"), None),
            "end" => note(pane, "say", "", &text(event, "answer"), None),
            _ => {}
        },
        "tool" => {
            let tool = text(event, "tool");
            match text(event, "phase").as_str() {
                "start" => note(pane, "tool", &tool, &text(event, "label"), None),
                "end" => {
                    let error = event.get("error").and_then(Value::as_bool).unwrap_or(false);
                    note(pane, "result", &tool, &text(event, "text"), Some(error));
                }
                _ => {}
            }
            return false;
        }
        _ => {}
    }
    let s = &mut pane.state;
    let now = Instant::now();
    match kind {
        "turn" => match text(event, "phase").as_str() {
            "start" => {
                s.turn_open = true;
                s.turn_at = now;
                s.turn_error = None;
            }
            "end" => {
                s.turn_open = false;
                s.turn_at = now;
                s.compacting = None;
                s.question = None;
                s.permissions.clear();
                s.permission_since = None;
                let reason = text(event, "reason");
                s.turn_error = matches!(reason.as_str(), "error" | "refusal").then_some(reason);
            }
            _ => return false,
        },
        "compact" => match text(event, "phase").as_str() {
            "start" => s.compacting = Some(now),
            "end" => s.compacting = None,
            _ => return false,
        },
        "permission" => {
            let id = text(event, "id");
            match text(event, "phase").as_str() {
                "ask" => {
                    if !s.permissions.iter().any(|(open, _)| *open == id) {
                        s.permissions.push((id, text(event, "tool")));
                        s.permission_since.get_or_insert(now);
                    }
                }
                "resolved" => {
                    s.permissions.retain(|(open, _)| *open != id);
                    if s.permissions.is_empty() {
                        s.permission_since = None;
                    }
                    resolve_request(&id, &text(event, "outcome"));
                }
                _ => return false,
            }
        }
        "question" => match text(event, "phase").as_str() {
            "start" => s.question = Some(text(event, "id")),
            "end" => s.question = None,
            _ => return false,
        },
        "usage" => {
            s.usage = Some(parse_usage(event));
            *statusline = true;
        }
        "background" => {
            s.background = event
                .get("tasks")
                .and_then(Value::as_array)
                .map(|tasks| {
                    tasks
                        .iter()
                        .map(|t| Task {
                            id: text(t, "id"),
                            kind: text(t, "type"),
                            status: text(t, "status"),
                            label: text(t, "label"),
                        })
                        .collect()
                })
                .unwrap_or_default();
            *statusline = true;
        }
        "row" => {
            pane.rows += 1;
            ROWS_WAKE.notify_waiters();
            return false;
        }
        _ => return false,
    }
    true
}

fn parse_usage(event: &Value) -> Usage {
    let ctx = event.get("context");
    let num = |v: Option<&Value>, key: &str| v.and_then(|v| v.get(key)).and_then(Value::as_f64);
    Usage {
        context_tokens: num(ctx, "tokens").map(|n| n as u64),
        context_window: num(ctx, "window").map(|n| n as u64),
        context_percent: num(ctx, "percent"),
        limits: event
            .get("limits")
            .and_then(Value::as_array)
            .map(|limits| {
                limits
                    .iter()
                    .filter_map(|l| {
                        Some(Limit {
                            kind: text(l, "kind"),
                            percent: l.get("percent").and_then(Value::as_f64)?,
                            resets_at: l.get("resets_at").and_then(Value::as_str).map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        cost_usd: event.get("cost_usd").and_then(Value::as_f64),
    }
}

/// 이 칸의 mod 사실 — 그 칸에 지금 도는 claude 가 hello 를 보낸 그 프로세스일 때만.
pub fn live(surface: &str) -> Option<ModState> {
    let state = PANES.lock().unwrap().get(surface).map(|p| p.state.clone())?;
    let shell = kasa_pty::lookup_session(surface)?.shell_pid()?;
    let table = kasa_pty::process_table_shared();
    let (kind, pid) = kasa_pty::agent_pid_for_shell(&table, shell)?;
    (kind == kasa_pty::AgentKind::Claude && pid == state.pid).then_some(state)
}

/// 보드의 백그라운드·서브에이전트 칸(라벨) — mod 칸일 때만. (백그라운드, 서브에이전트).
pub fn task_labels(surface: &str) -> Option<(Vec<String>, Vec<String>)> {
    let state = live(surface)?;
    let (subagents, background): (Vec<&Task>, Vec<&Task>) = state.running_tasks().partition(|t| t.kind == "subagent");
    let label = |t: &&Task| if t.label.is_empty() { t.kind.clone() } else { t.label.clone() };
    Some((background.iter().map(label).collect(), subagents.iter().map(label).collect()))
}

/// 닫힌 칸의 사실을 잊는다 — 앱이 살아 있는 칸 목록으로 부른다.
pub fn retain(live: &[String]) {
    PANES.lock().unwrap().retain(|id, _| live.contains(id));
}

/// 최근 활동(프롬프트·답·도구·결과), 오래된 것부터. `collab.activity` 의 mod 판 — 칸이 mod 칸일 때만.
pub fn activity(surface: &str, limit: usize) -> Option<Vec<Value>> {
    live(surface)?;
    let panes = PANES.lock().unwrap();
    let pane = panes.get(surface)?;
    let skip = pane.activity.len().saturating_sub(limit);
    Some(pane.activity.iter().skip(skip).cloned().collect())
}

static ROWS_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// 이 칸의 대화 행 수(mod 가 없으면 None).
pub fn row_count(surface: &str) -> Option<u64> {
    PANES.lock().unwrap().get(surface).map(|p| p.rows)
}

/// 대화 행이 `seen` 보다 많아지거나 `wait` 가 지날 때까지 기다린다. mod 가 없는 칸은 바로 돌아온다.
pub async fn wait_rows(surface: &str, seen: u64, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = ROWS_WAKE.notified();
        if row_count(surface).is_none_or(|n| n > seen) || tokio::time::Instant::now() >= deadline {
            return;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return;
        }
    }
}

/// 상태줄(`kasaterm-cli statusline`)이 읽는 칸별 파일 — 매초 도는 상태줄이 앱에 묻지 않게.
fn write_statusline(surface: &str) {
    let body = {
        let panes = PANES.lock().unwrap();
        let Some(pane) = panes.get(surface) else { return };
        let s = &pane.state;
        let usage = s.usage.clone().unwrap_or_default();
        json!({
            "session": s.session,
            "at_ms": now_ms(),
            "context_percent": usage.context_percent,
            "cost_usd": usage.cost_usd,
            "limits": usage.limits.iter().map(|l| json!({"kind": l.kind, "percent": l.percent, "resets_at": l.resets_at})).collect::<Vec<_>>(),
            "background": s.running_tasks().count(),
        })
    };
    let dir = std::env::temp_dir().join("kasaterm-statusline");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-mod.json", surface.trim_start_matches('%')));
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

// ── 승인 요청 ────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Ask {
    id: String,
    surface: String,
    session: String,
    tool: String,
    input: Value,
    input_truncated: bool,
    preview: String,
    cwd: String,
    agent: Option<String>,
    created_ms: u64,
    expires: Instant,
    expires_ms: u64,
    decision: Option<(String, String, String)>,
    closed: bool,
}

impl Ask {
    fn view(&self) -> Value {
        json!({
            "id": self.id, "surface": self.surface, "session": self.session, "tool": self.tool,
            "input": self.input, "input_truncated": self.input_truncated, "preview": self.preview,
            "cwd": self.cwd, "agent": self.agent,
            "created_at_ms": self.created_ms, "expires_at_ms": self.expires_ms,
        })
    }
}

static REQUESTS: LazyLock<Mutex<HashMap<String, Ask>>> = LazyLock::new(Default::default);
static REQUESTS_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static OPENED: LazyLock<tokio::sync::broadcast::Sender<Value>> = LazyLock::new(|| tokio::sync::broadcast::channel(64).0);

/// 새 승인 요청이 열릴 때마다 그 요청(`permissions()` 의 한 줄)을 받는다 — 푸시 알림을 붙이는 자리.
pub fn subscribe() -> tokio::sync::broadcast::Receiver<Value> {
    OPENED.subscribe()
}

fn sweep_requests(map: &mut HashMap<String, Ask>) {
    let now = Instant::now();
    for request in map.values_mut() {
        if now >= request.expires {
            request.closed = true;
        }
    }
    map.retain(|_, r| !r.closed || now < r.expires + Duration::from_secs(60));
}

/// 지금 열린(결정 전·만료 전) 요청들.
pub fn permissions() -> Vec<Value> {
    let mut map = REQUESTS.lock().unwrap();
    sweep_requests(&mut map);
    let mut open: Vec<&Ask> = map.values().filter(|r| !r.closed && r.decision.is_none()).collect();
    open.sort_by_key(|r| r.created_ms);
    open.into_iter().map(Ask::view).collect()
}

/// 원격(또는 이 기계의 다른 화면)의 결정. 같은 칸·세션의 열린 요청에만 닿는다.
pub fn decide(id: &str, surface: &str, session: &str, decision: &str, message: &str, by: &str) -> Result<(), &'static str> {
    if !matches!(decision, "allow" | "deny") {
        return Err("decision");
    }
    let request = {
        let mut map = REQUESTS.lock().unwrap();
        sweep_requests(&mut map);
        let request = map.get_mut(id).ok_or("unknown")?;
        if request.surface != surface || request.session != session {
            return Err("mismatch");
        }
        if request.closed {
            return Err("expired");
        }
        if request.decision.is_some() {
            return Err("decided");
        }
        request.decision = Some((decision.into(), message.into(), by.into()));
        request.clone()
    };
    audit(&request, decision, by);
    REQUESTS_WAKE.notify_waiters();
    Ok(())
}

fn resolve_request(id: &str, outcome: &str) {
    let local = {
        let mut map = REQUESTS.lock().unwrap();
        match map.get_mut(id) {
            Some(request) if !request.closed => {
                request.closed = true;
                request.decision.is_none().then(|| request.clone())
            }
            _ => None,
        }
    };
    if let Some(request) = local {
        audit(&request, &format!("local:{outcome}"), "engine");
    }
    REQUESTS_WAKE.notify_waiters();
}

/// 자리에서 답했다 — 그 칸에서 `key` 전에 열린 요청을 닫아 원격 화면에서 거둔다(엔진은 답한 순간을 안 알린다).
pub fn close_answered(surface: &str, key: Instant) {
    let closed: Vec<Ask> = {
        let mut map = REQUESTS.lock().unwrap();
        map.values_mut()
            .filter(|r| r.surface == surface && !r.closed && r.decision.is_none() && r.expires.checked_sub(PERMISSION_TTL).is_some_and(|at| at < key))
            .map(|r| {
                r.closed = true;
                r.clone()
            })
            .collect()
    };
    if closed.is_empty() {
        return;
    }
    for request in &closed {
        audit(request, "local:answered", "desk");
    }
    REQUESTS_WAKE.notify_waiters();
}

fn audit(request: &Ask, decision: &str, by: &str) {
    if cfg!(test) {
        return;
    }
    let root = kasa_socket::collab_root().join("claude-mod");
    let _ = std::fs::create_dir_all(&root);
    let line = json!({
        "at_ms": now_ms(), "surface": request.surface, "session": request.session, "id": request.id,
        "tool": request.tool, "input_fnv": kasa_socket::tell::fingerprint(&request.input.to_string()),
        "decision": decision, "by": by,
    });
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(root.join("permission-audit.jsonl")) {
        let _ = writeln!(file, "{line}");
    }
}

/// mod 의 긴 폴링. 요청을 열고(이미 열렸으면 그대로) 결정·끝남·`HOLD` 중 먼저 오는 것을 답한다.
pub async fn open_and_wait(surface: &str, session: &str, body: &Value, hold: Duration) -> Value {
    let id = text(body, "id");
    if id.is_empty() {
        return json!({"decision": "none", "pending": false, "error": "id"});
    }
    let opened = {
        let mut map = REQUESTS.lock().unwrap();
        sweep_requests(&mut map);
        if map.contains_key(&id) {
            None
        } else {
            let (input, input_truncated) = cap_input(body.get("input").cloned().unwrap_or(Value::Null));
            let created_ms = body.get("created_at_ms").and_then(Value::as_u64).unwrap_or_else(now_ms);
            let request = Ask {
                id: id.clone(),
                surface: surface.into(),
                session: session.into(),
                tool: text(body, "tool"),
                input,
                input_truncated,
                preview: text(body, "preview").chars().take(400).collect(),
                cwd: text(body, "cwd"),
                agent: body.get("agent").and_then(Value::as_str).map(str::to_string),
                created_ms,
                expires: Instant::now() + PERMISSION_TTL,
                expires_ms: now_ms() + PERMISSION_TTL.as_millis() as u64,
                decision: None,
                closed: false,
            };
            let view = request.view();
            map.insert(id.clone(), request);
            Some(view)
        }
    };
    if let Some(view) = opened {
        apply(surface, session, &[json!({"kind": "permission", "phase": "ask", "id": id, "tool": text(body, "tool")})]);
        let _ = OPENED.send(view);
    }
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let notified = REQUESTS_WAKE.notified();
        let answer = {
            let mut map = REQUESTS.lock().unwrap();
            sweep_requests(&mut map);
            match map.get(&id) {
                None => Some(json!({"decision": "none", "pending": false})),
                Some(r) if r.surface != surface || r.session != session => {
                    Some(json!({"decision": "none", "pending": false, "error": "mismatch"}))
                }
                Some(r) => match &r.decision {
                    Some((decision, message, by)) => Some(json!({"decision": decision, "message": message, "by": by, "pending": false})),
                    None if r.closed => Some(json!({"decision": "none", "pending": false})),
                    None => None,
                },
            }
        };
        if let Some(answer) = answer {
            return answer;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return json!({"decision": "none", "pending": true});
        }
    }
}

fn cap_input(input: Value) -> (Value, bool) {
    let raw = input.to_string();
    if raw.len() <= INPUT_CAP {
        return (input, false);
    }
    let mut cut = INPUT_CAP;
    while !raw.is_char_boundary(cut) {
        cut -= 1;
    }
    (Value::String(raw[..cut].to_string()), true)
}

// ── tell 받은편지함 ───────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Letter {
    id: String,
    session: String,
    body: String,
    offered: Instant,
    taken: Option<Instant>,
}

static INBOX: LazyLock<Mutex<HashMap<String, Vec<Letter>>>> = LazyLock::new(Default::default);
static INBOX_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

type AckListener = Arc<dyn Fn(&str, &str, bool) + Send + Sync>;
static ACK_LISTENER: OnceLock<AckListener> = OnceLock::new();

/// mod 가 tell 을 넣었다고 알린 칸·메시지를 앱에 넘길 자리(창 이름 바꾸기 등). 앱이 한 번 건다.
pub fn set_ack_listener(listener: impl Fn(&str, &str, bool) + Send + Sync + 'static) {
    let _ = ACK_LISTENER.set(Arc::new(listener));
}

/// tell 하나를 그 칸의 mod 에게 맡긴다. 영수증은 이미 dispatching 이고, mod 의 ack 가 끝맺는다.
pub fn offer(surface: &str, session: &str, id: &str, body: &str) {
    let mut inbox = INBOX.lock().unwrap();
    let letters = inbox.entry(surface.to_string()).or_default();
    if !letters.iter().any(|l| l.id == id) {
        letters.push(Letter { id: id.into(), session: session.into(), body: body.into(), offered: Instant::now(), taken: None });
    }
    drop(inbox);
    INBOX_WAKE.notify_waiters();
}

/// 이 칸에 맡겨 둔(아직 끝나지 않은) tell 이 있나 — 있으면 같은 칸의 다음 tell 을 꺼내지 않는다.
pub fn has_offer(surface: &str) -> bool {
    INBOX.lock().unwrap().get(surface).is_some_and(|l| !l.is_empty())
}

/// 오래된 맡김을 정리한다: 안 꺼내 간 것은 대기열로 되돌리고(넣지 않은 것이 확실), 꺼내 갔는데 답이 없는
/// 것은 uncertain 으로 끝낸다. 앱의 tell 틱이 부른다.
pub fn sweep_inbox() {
    let mut done: Vec<(String, kasa_socket::tell::State, &'static str)> = Vec::new();
    {
        let mut inbox = INBOX.lock().unwrap();
        for letters in inbox.values_mut() {
            letters.retain(|l| {
                match l.taken {
                    None if l.offered.elapsed() >= OFFER_UNTAKEN => {
                        done.push((l.id.clone(), kasa_socket::tell::State::Accepted, "mod did not take it while the receiver rested; requeued"));
                        false
                    }
                    Some(at) if at.elapsed() >= OFFER_UNACKED => {
                        done.push((l.id.clone(), kasa_socket::tell::State::Uncertain, "mod took it but never confirmed the submit; automatic retry prohibited"));
                        false
                    }
                    _ => true,
                }
            });
        }
        inbox.retain(|_, l| !l.is_empty());
    }
    for (id, state, reason) in done {
        let _ = crate::tell_service::transition(&id, state, reason);
    }
}

/// mod 의 긴 폴링 — 이 칸·세션에 맡긴 것 중 아직 안 꺼낸 것을 꺼내 간다.
pub async fn take(surface: &str, session: &str, hold: Duration) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let notified = INBOX_WAKE.notified();
        let taken: Vec<Value> = {
            let mut inbox = INBOX.lock().unwrap();
            inbox
                .get_mut(surface)
                .map(|letters| {
                    letters
                        .iter_mut()
                        .filter(|l| l.taken.is_none() && l.session == session)
                        .map(|l| {
                            l.taken = Some(Instant::now());
                            json!({"id": l.id, "body": l.body})
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        if !taken.is_empty() {
            return taken;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return Vec::new();
        }
    }
}

/// mod 가 넣은 결과. 영수증을 끝맺는다.
pub fn ack(surface: &str, session: &str, id: &str, submitted: bool, reason: &str) -> Result<(), &'static str> {
    {
        let mut inbox = INBOX.lock().unwrap();
        let letters = inbox.get_mut(surface).ok_or("unknown")?;
        let at = letters.iter().position(|l| l.id == id && l.session == session).ok_or("unknown")?;
        letters.remove(at);
    }
    let (state, reason) = if submitted {
        (kasa_socket::tell::State::Submitted, "submitted by the in-session mod as a prompt of its own; model read is unconfirmed".to_string())
    } else {
        (kasa_socket::tell::State::Failed, format!("in-session mod refused the submit: {reason}"))
    };
    let _ = crate::tell_service::transition(id, state, &reason);
    if let Some(listener) = ACK_LISTENER.get() {
        listener(surface, id, submitted);
    }
    Ok(())
}

// ── HTTP ─────────────────────────────────────────────────────────────────

async fn local_body(req: HttpRequest) -> Result<Value, Response> {
    if crate::http::is_remote_peer(&req) {
        return Err((StatusCode::FORBIDDEN, "claude-mod routes are loopback only").into_response());
    }
    let bytes: Bytes = axum::body::to_bytes(req.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response())?;
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|_| (StatusCode::BAD_REQUEST, "body is not JSON").into_response())
}

fn hold_of(q: &HashMap<String, String>) -> Duration {
    q.get("wait_ms").and_then(|w| w.parse::<u64>().ok()).map(Duration::from_millis).unwrap_or(HOLD).min(HOLD)
}

/// `/claude-mod/*` 경로. http.rs 의 큰 라우터에 그대로 붙는다.
pub fn routes() -> Router {
    Router::new()
        .route(
            "/claude-mod/event",
            post(|req: HttpRequest| async move {
                let body = match local_body(req).await {
                    Ok(body) => body,
                    Err(r) => return r,
                };
                let events = body.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
                apply(&text(&body, "surface"), &text(&body, "session"), &events);
                Json(json!({"ok": true})).into_response()
            }),
        )
        .route(
            "/claude-mod/permission",
            post(|Query(q): Query<HashMap<String, String>>, req: HttpRequest| async move {
                let body = match local_body(req).await {
                    Ok(body) => body,
                    Err(r) => return r,
                };
                let request = body.get("request").cloned().unwrap_or(Value::Null);
                Json(open_and_wait(&text(&body, "surface"), &text(&body, "session"), &request, hold_of(&q)).await).into_response()
            }),
        )
        .route(
            "/claude-mod/permissions",
            get(|req: HttpRequest| async move {
                if let Err(r) = local_body(req).await {
                    return r;
                }
                Json(json!({"requests": permissions()})).into_response()
            }),
        )
        .route(
            "/claude-mod/permission/decide",
            post(|req: HttpRequest| async move {
                let body = match local_body(req).await {
                    Ok(body) => body,
                    Err(r) => return r,
                };
                let result = decide(
                    &text(&body, "id"),
                    &text(&body, "surface"),
                    &text(&body, "session"),
                    &text(&body, "decision"),
                    &text(&body, "message"),
                    &text(&body, "by"),
                );
                Json(match result {
                    Ok(()) => json!({"ok": true}),
                    Err(error) => json!({"ok": false, "error": error}),
                })
                .into_response()
            }),
        )
        .route(
            "/claude-mod/inbox",
            get(|Query(q): Query<HashMap<String, String>>, req: HttpRequest| async move {
                if let Err(r) = local_body(req).await {
                    return r;
                }
                let surface = q.get("surface").cloned().unwrap_or_default();
                let session = q.get("session").cloned().unwrap_or_default();
                Json(json!({"messages": take(&surface, &session, hold_of(&q)).await})).into_response()
            }),
        )
        .route(
            "/claude-mod/inbox/ack",
            post(|req: HttpRequest| async move {
                let body = match local_body(req).await {
                    Ok(body) => body,
                    Err(r) => return r,
                };
                let submitted = text(&body, "state") == "submitted";
                let result = ack(&text(&body, "surface"), &text(&body, "session"), &text(&body, "id"), submitted, &text(&body, "reason"));
                Json(match result {
                    Ok(()) => json!({"ok": true}),
                    Err(error) => json!({"ok": false, "error": error}),
                })
                .into_response()
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(v: Value) -> Vec<Value> {
        vec![v]
    }

    #[test]
    fn events_before_hello_or_from_another_session_are_dropped() {
        apply("%t1", "s1", &ev(json!({"kind": "turn", "phase": "start"})));
        assert!(PANES.lock().unwrap().get("%t1").is_none());
        apply("%t1", "s1", &ev(json!({"kind": "hello", "pid": 7, "mod": "0.1.0"})));
        apply("%t1", "s2", &ev(json!({"kind": "turn", "phase": "start"})));
        assert!(!PANES.lock().unwrap()["%t1"].state.turn_open);
        apply("%t1", "s1", &ev(json!({"kind": "turn", "phase": "start"})));
        assert!(PANES.lock().unwrap()["%t1"].state.turn_open);
    }

    #[test]
    fn a_turn_that_ends_in_error_is_remembered_until_the_next_turn() {
        apply("%t2", "s", &ev(json!({"kind": "hello", "pid": 1})));
        apply("%t2", "s", &[json!({"kind": "turn", "phase": "start"}), json!({"kind": "turn", "phase": "end", "reason": "error"})]);
        assert_eq!(PANES.lock().unwrap()["%t2"].state.turn_error.as_deref(), Some("error"));
        apply("%t2", "s", &ev(json!({"kind": "turn", "phase": "start"})));
        assert_eq!(PANES.lock().unwrap()["%t2"].state.turn_error, None);
    }

    #[test]
    fn activity_reads_like_the_transcript_activity() {
        apply("%t10", "s", &[
            json!({"kind": "hello", "pid": 1}),
            json!({"kind": "turn", "phase": "start", "text": "고쳐 줘"}),
            json!({"kind": "tool", "phase": "start", "tool": "Bash", "label": "cargo test"}),
            json!({"kind": "tool", "phase": "end", "tool": "Bash", "error": true, "text": "1 failed"}),
            json!({"kind": "turn", "phase": "end", "answer": "고쳤어요"}),
        ]);
        let panes = PANES.lock().unwrap();
        let kinds: Vec<_> = panes["%t10"].activity.iter().map(|a| a["kind"].as_str().unwrap().to_string()).collect();
        assert_eq!(kinds, ["prompt", "tool", "result", "say"]);
        assert_eq!(panes["%t10"].activity[2]["is_error"], true);
    }

    #[test]
    fn resting_needs_no_turn_permission_question_or_compaction() {
        let mut s = ModState::new("s", 1, "");
        assert!(s.resting());
        s.permissions.push(("p".into(), "Bash".into()));
        assert!(!s.resting());
        s.permissions.clear();
        s.question = Some("q".into());
        assert!(!s.resting());
    }

    #[test]
    fn a_new_hello_with_another_session_starts_over() {
        apply("%t3", "a", &[json!({"kind": "hello", "pid": 1}), json!({"kind": "turn", "phase": "start"})]);
        apply("%t3", "b", &ev(json!({"kind": "hello", "pid": 1})));
        let panes = PANES.lock().unwrap();
        assert_eq!(panes["%t3"].state.session, "b");
        assert!(!panes["%t3"].state.turn_open);
    }

    #[tokio::test]
    async fn a_new_row_wakes_the_conversation_reader() {
        apply("%t4", "s", &ev(json!({"kind": "hello", "pid": 1})));
        assert_eq!(row_count("%t4"), Some(0));
        let wait = tokio::spawn(async { wait_rows("%t4", 0, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        apply("%t4", "s", &ev(json!({"kind": "row", "uuid": "u1"})));
        tokio::time::timeout(Duration::from_secs(1), wait).await.unwrap().unwrap();
        assert_eq!(row_count("%t4"), Some(1));
        assert_eq!(row_count("%none"), None);
    }

    #[tokio::test]
    async fn a_decision_reaches_the_waiting_mod_and_only_once() {
        apply("%t5", "s", &ev(json!({"kind": "hello", "pid": 1})));
        let request = json!({"id": "toolu_x", "tool": "Bash", "input": {"command": "ls"}, "preview": "ls"});
        let wait = tokio::spawn(async move { open_and_wait("%t5", "s", &request, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(permissions().iter().any(|r| r["id"] == "toolu_x"));
        assert!(PANES.lock().unwrap()["%t5"].state.permissions.iter().any(|(id, _)| id == "toolu_x"));
        assert_eq!(decide("toolu_x", "%t9", "s", "allow", "", "test"), Err("mismatch"));
        assert_eq!(decide("toolu_x", "%t5", "s", "allow", "", "test"), Ok(()));
        assert_eq!(decide("toolu_x", "%t5", "s", "deny", "", "test"), Err("decided"));
        let answer = wait.await.unwrap();
        assert_eq!(answer["decision"], "allow");
        assert!(!permissions().iter().any(|r| r["id"] == "toolu_x"));
    }

    #[tokio::test]
    async fn a_locally_answered_request_releases_the_poll() {
        apply("%t6", "s", &ev(json!({"kind": "hello", "pid": 1})));
        let request = json!({"id": "toolu_y", "tool": "Edit"});
        let wait = tokio::spawn(async move { open_and_wait("%t6", "s", &request, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        apply("%t6", "s", &ev(json!({"kind": "permission", "phase": "resolved", "id": "toolu_y", "outcome": "ran"})));
        let answer = wait.await.unwrap();
        assert_eq!(answer, json!({"decision": "none", "pending": false}));
        assert!(PANES.lock().unwrap()["%t6"].state.permissions.is_empty());
        assert_eq!(decide("toolu_y", "%t6", "s", "allow", "", "late"), Err("expired"));
    }

    #[tokio::test]
    async fn a_key_at_the_desk_withdraws_the_request_from_remote_screens() {
        let request = json!({"id": "toolu_k", "tool": "Bash"});
        let wait = tokio::spawn(async move { open_and_wait("%t11", "s", &request, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(30)).await;
        close_answered("%t11", Instant::now());
        assert_eq!(wait.await.unwrap(), json!({"decision": "none", "pending": false}));
        assert!(!permissions().iter().any(|r| r["id"] == "toolu_k"));
    }

    #[tokio::test]
    async fn an_undecided_poll_says_pending_after_its_hold() {
        let request = json!({"id": "toolu_z", "tool": "Bash"});
        let answer = open_and_wait("%t7", "s", &request, Duration::from_millis(30)).await;
        assert_eq!(answer, json!({"decision": "none", "pending": true}));
    }

    #[tokio::test]
    async fn the_inbox_hands_a_letter_once_to_the_matching_session() {
        offer("%t8", "s", "kt1.1.a", "hello");
        assert!(take("%t8", "other", Duration::from_millis(10)).await.is_empty());
        let got = take("%t8", "s", Duration::from_millis(10)).await;
        assert_eq!(got, vec![json!({"id": "kt1.1.a", "body": "hello"})]);
        assert!(take("%t8", "s", Duration::from_millis(10)).await.is_empty());
        assert!(has_offer("%t8"));
    }

    #[test]
    fn oversized_input_is_cut_on_a_char_boundary() {
        let (input, cut) = cap_input(json!({"command": "가".repeat(INPUT_CAP)}));
        assert!(cut);
        assert!(input.as_str().unwrap().len() <= INPUT_CAP);
    }
}
