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
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 승인 요청 하나를 쥐는 한도. 넘으면 원격 결정을 안 받고 엔진 창만 남는다.
pub const PERMISSION_TTL: Duration = Duration::from_secs(600);
/// 긴 폴링 한 번이 쥐는 시간. mod 는 같은 요청·같은 칸으로 다시 연다.
const HOLD: Duration = Duration::from_secs(25);
/// 꺼내 간 거울 입력의 결과(ack)를 기다리는 한도. 넘으면 버린다 — 다시 내주면 두 번 들어갈 수 있다.
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

    pub fn running_tasks(&self) -> impl Iterator<Item = &Task> {
        self.background.iter().filter(|t| matches!(t.status.as_str(), "running" | "pending"))
    }
}

struct Pane {
    state: ModState,
    activity: VecDeque<Value>,
    /// 대화 행이 몇 번 쌓였나 — 대화 보기가 기록 파일을 다시 읽을 때를 안다(내용은 파일이 정본).
    rows: u64,
    status: Option<StatusLine>,
    focus: FocusTrack,
}

/// 엔진이 자기 상태줄을 다시 그리기를 기다리는 한도. 엔진은 refreshInterval 1초에 300ms 를 더 미뤄,
/// 실측 늦을 때 2.5초였다 — 그보다 넉넉히 쥐되 엔진이 멈춰도 옛 줄이 남지 않게 끝을 둔다.
pub const STATUS_HOLD: Duration = Duration::from_secs(5);

/// 상태줄 mod 가 모델·effort·경로·브랜치·문맥이 바뀐 순간 지은 줄(ANSI). 엔진의 상태줄 명령은 1초 남짓
/// 뒤에야 다시 돌아서, 그 사이만 화면이 이 줄을 덧그린다(`status_overlay`) — 엔진 줄이 늘 정본이다.
struct StatusLine {
    line: String,
    at: Instant,
    /// 이 줄을 처음 덧그릴 때 화면의 엔진 줄. 엔진 줄이 이것과 달라지면 엔진이 다시 그린 것이다.
    base: Option<String>,
    released: bool,
}

static PANES: LazyLock<Mutex<HashMap<String, Pane>>> = LazyLock::new(Default::default);

type Listener = Arc<dyn Fn(&str) + Send + Sync>;
static LISTENER: OnceLock<Listener> = OnceLock::new();
static STATUS_LISTENER: OnceLock<Listener> = OnceLock::new();

/// 사실이 바뀐 칸을 앱에 알릴 자리(상태 판정 다시·GUI 깨우기). 앱이 한 번 건다.
pub fn set_listener(listener: impl Fn(&str) + Send + Sync + 'static) {
    let _ = LISTENER.set(Arc::new(listener));
}

/// 새 상태줄이 온 칸을 알릴 자리 — 판정·보드는 그대로 두고 화면만 다시 그리면 된다.
pub fn set_status_listener(listener: impl Fn(&str) + Send + Sync + 'static) {
    let _ = STATUS_LISTENER.set(Arc::new(listener));
}

/// 화면이 이 칸 상태줄 행에 지금 덧그릴 줄. `engine` 은 그 행에 엔진이 그려 둔 글자(표식 뒤부터)다.
/// 엔진이 그 뒤 자기 줄을 다시 그렸거나 한도가 지났으면 손을 뗀다.
pub fn status_overlay(surface: &str, engine: &str) -> Option<String> {
    let mut panes = PANES.lock().unwrap();
    let status = panes.get_mut(surface)?.status.as_mut()?;
    if status.released || status.at.elapsed() > STATUS_HOLD {
        return None;
    }
    let base = status.base.get_or_insert_with(|| engine.to_string());
    if base != engine {
        status.released = true;
        return None;
    }
    Some(status.line.clone())
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
    let mut git = Vec::new();
    let mut status = false;
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
                        Pane {
                            state: ModState::new(session, pid, &text(event, "mod")),
                            activity: VecDeque::new(),
                            rows: 0,
                            status: None,
                            focus: FocusTrack::default(),
                        },
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
            if kind == "git" {
                git.push(git_signal(surface, event));
                continue;
            }
            if kind == "status" {
                let line = text(event, "line");
                if !line.is_empty() {
                    pane.status = Some(StatusLine { line, at: Instant::now(), base: None, released: false });
                    status = true;
                }
                continue;
            }
            touched |= apply_one(pane, &kind, event, &mut statusline);
        }
    }
    if !events.is_empty() {
        bump(surface);
    }
    if !git.is_empty() {
        record_git(git);
    }
    if statusline {
        write_statusline(surface);
    }
    if touched {
        changed(surface);
    }
    if status {
        if let Some(listener) = STATUS_LISTENER.get() {
            listener(surface);
        }
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
            "end" => {
                note(pane, "say", "", &text(event, "answer"), None);
                pane.focus.tools.clear();
            }
            _ => {}
        },
        "tool" => {
            let tool = text(event, "tool");
            match text(event, "phase").as_str() {
                "start" => {
                    note(pane, "tool", &tool, &text(event, "label"), None);
                    pane.focus.start(event, tool);
                }
                "end" => {
                    let error = event.get("error").and_then(Value::as_bool).unwrap_or(false);
                    note(pane, "result", &tool, &text(event, "text"), Some(error));
                    pane.focus.end(&text(event, "id"), &tool, error);
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
            pane.focus.task_since.retain(|id, _| s.background.iter().any(|t| t.id == *id));
            for task in &s.background {
                pane.focus.task_since.entry(task.id.clone()).or_insert(now);
            }
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

// ── Info 카드 ────────────────────────────────────────────────────────────

const RECENT_TOOLS: usize = 5;

/// 오른쪽 Info 열 「지금 보는 칸」이 그리는 mod 몫 — 도는 도구·끝난 도구·백그라운드가 언제부터인가.
#[derive(Default)]
struct FocusTrack {
    tools: Vec<OpenTool>,
    recent: VecDeque<DoneTool>,
    /// 백그라운드 작업을 처음 본 때. 엔진 목록은 시작 시각을 안 싣는다.
    task_since: HashMap<String, Instant>,
}

struct OpenTool {
    id: String,
    tool: String,
    label: String,
    agent: bool,
    since: Instant,
}

struct DoneTool {
    tool: String,
    label: String,
    agent: bool,
    error: bool,
    took: Duration,
    at: Instant,
}

impl FocusTrack {
    fn start(&mut self, event: &Value, tool: String) {
        let id = text(event, "id");
        self.tools.retain(|t| id.is_empty() || t.id != id);
        let agent = event.get("agent").and_then(Value::as_str).is_some_and(|a| !a.is_empty());
        self.tools.push(OpenTool { id, tool, label: text(event, "label"), agent, since: Instant::now() });
    }

    fn end(&mut self, id: &str, tool: &str, error: bool) {
        let Some(at) = self.tools.iter().position(|t| t.id == id && t.tool == tool) else { return };
        let open = self.tools.remove(at);
        if self.recent.len() >= RECENT_TOOLS {
            self.recent.pop_back();
        }
        let now = Instant::now();
        self.recent.push_front(DoneTool {
            tool: open.tool,
            label: open.label,
            agent: open.agent,
            error,
            took: now.duration_since(open.since),
            at: now,
        });
    }
}

/// 카드 한 줄의 도구. 나이·걸린 시간은 부른 순간 기준(ms)이라 다른 기기로 건너가도 시계가 안 갈린다.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusTool {
    pub tool: String,
    pub label: String,
    /// 서브에이전트 안에서 돈 것.
    pub agent: bool,
    pub age_ms: u64,
    /// 끝난 도구만 — 걸린 시간과 실패.
    pub took_ms: Option<u64>,
    pub error: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusTask {
    /// `subagent`·`shell`·`monitor`·`workflow`.
    pub kind: String,
    pub label: String,
    pub status: String,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusAsk {
    pub tool: String,
    pub preview: String,
    pub age_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusContext {
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    pub percent: Option<f64>,
}

/// 한 칸의 mod 사실 중 Info 카드가 쓰는 것. `state` 는 `working`·`resting`·`permission`·`question`·`compacting`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusFacts {
    pub state: String,
    pub tools: Vec<FocusTool>,
    /// 끝난 도구, 최근 것부터.
    pub recent: Vec<FocusTool>,
    pub background: Vec<FocusTask>,
    pub context: Option<FocusContext>,
    pub ask: Option<FocusAsk>,
}

/// Info 카드의 mod 몫 — 그 칸이 mod 칸일 때만.
pub fn focus_facts(surface: &str) -> Option<FocusFacts> {
    facts_of(surface, &live(surface)?)
}

fn facts_of(surface: &str, state: &ModState) -> Option<FocusFacts> {
    let now = Instant::now();
    let ms = |d: Duration| d.as_millis() as u64;
    let ask = permissions()
        .into_iter()
        .find(|r| r["surface"] == surface && r["session"] == state.session.as_str())
        .map(|r| FocusAsk {
            tool: text(&r, "tool"),
            preview: text(&r, "preview"),
            age_ms: now_ms().saturating_sub(r["created_at_ms"].as_u64().unwrap_or_else(now_ms)),
        });
    let phase = if !state.permissions.is_empty() || ask.is_some() {
        "permission"
    } else if state.question.is_some() {
        "question"
    } else if state.compacting.is_some() {
        "compacting"
    } else if state.turn_open {
        "working"
    } else {
        "resting"
    };
    let panes = PANES.lock().unwrap();
    let track = &panes.get(surface)?.focus;
    Some(FocusFacts {
        state: phase.into(),
        tools: track
            .tools
            .iter()
            .map(|t| FocusTool { tool: t.tool.clone(), label: t.label.clone(), agent: t.agent, age_ms: ms(now - t.since), took_ms: None, error: false })
            .collect(),
        recent: track
            .recent
            .iter()
            .map(|t| FocusTool {
                tool: t.tool.clone(),
                label: t.label.clone(),
                agent: t.agent,
                age_ms: ms(now - t.at),
                took_ms: Some(ms(t.took)),
                error: t.error,
            })
            .collect(),
        background: state
            .running_tasks()
            .map(|t| FocusTask {
                kind: t.kind.clone(),
                label: t.label.clone(),
                status: t.status.clone(),
                age_ms: track.task_since.get(&t.id).map_or(0, |since| ms(now - *since)),
            })
            .collect(),
        context: state.usage.as_ref().map(|u| FocusContext { tokens: u.context_tokens, window: u.context_window, percent: u.context_percent }),
        ask,
    })
}

static SEQS: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Default::default);
static SEQ_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
static FOCUS_LISTENER: OnceLock<Listener> = OnceLock::new();

/// mod 사실이 바뀐 칸마다 부를 자리 — 도구 하나가 시작·끝나도 부른다(`set_listener` 는 판정이 바뀔 때만). 앱이 한 번 건다.
pub fn set_focus_listener(listener: impl Fn(&str) + Send + Sync + 'static) {
    let _ = FOCUS_LISTENER.set(Arc::new(listener));
}

/// 그 칸의 mod 사실이 바뀌었다 — 기다리는 쪽(GUI·다른 기기의 긴 폴링)을 깨운다.
fn bump(surface: &str) {
    *SEQS.lock().unwrap().entry(surface.to_string()).or_default() += 1;
    SEQ_WAKE.notify_waiters();
    if let Some(listener) = FOCUS_LISTENER.get() {
        listener(surface);
    }
}

/// 그 칸의 mod 사실 판 번호. 바뀐 적 없으면 0.
pub fn seq(surface: &str) -> u64 {
    SEQS.lock().unwrap().get(surface).copied().unwrap_or(0)
}

/// 그 칸의 판 번호가 `seen` 과 달라지거나 `wait` 가 지날 때까지 기다린다.
pub async fn wait_seq(surface: &str, seen: u64, wait: Duration) {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = SEQ_WAKE.notified();
        if seq(surface) != seen || tokio::time::Instant::now() >= deadline {
            return;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return;
        }
    }
}

/// 거울(다른 기기·폰)이 대화 보기 위에 덧붙일 이 칸의 지금 — 일 상태, 도는 도구, 열린 승인 요청(입력 원문까지),
/// 질문. 기록 파일은 메시지가 끝나야 써지니 이것으로 기다림 없이 그린다. mod 칸이 아니면 `live: false`.
pub fn mirror_view(surface: &str) -> Value {
    let seq = seq(surface);
    let Some(state) = live(surface) else {
        return json!({"live": false, "seq": seq});
    };
    let running: Vec<Value> = PANES
        .lock()
        .unwrap()
        .get(surface)
        .map(|p| {
            p.focus
                .tools
                .iter()
                .map(|t| json!({"id": t.id, "tool": t.tool, "label": t.label, "agent": t.agent}))
                .collect()
        })
        .unwrap_or_default();
    let permissions: Vec<Value> = permissions()
        .into_iter()
        .filter(|r| r["surface"] == surface && r["session"] == state.session.as_str())
        .collect();
    json!({
        "live": true, "seq": seq, "session": state.session,
        "turn_open": state.turn_open, "compacting": state.compacting.is_some(),
        "question": state.question, "tools": running, "permissions": permissions,
    })
}

/// 「깃이 바뀌었을 수 있다」 — mod 가 파일을 고친 도구·쓰는 명령 뒤에 알린 것. 내용 없이 칸·폴더·경로만.
/// Git 열은 이것으로 주기를 기다리지 않고 바로 다시 읽는다(GUI 는 `set_git_listener`, 다른 기기는 `wait_git`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSignal {
    pub seq: u64,
    pub surface: String,
    pub cwd: String,
    pub paths: Vec<String>,
}

const GIT_SIGNAL_CAP: usize = 64;
const GIT_PATH_CAP: usize = 32;

#[derive(Default)]
struct GitSignals {
    seq: u64,
    recent: VecDeque<GitSignal>,
}

static GIT_SIGNALS: LazyLock<Mutex<GitSignals>> = LazyLock::new(Default::default);
static GIT_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
type GitListener = Arc<dyn Fn() + Send + Sync>;
static GIT_LISTENER: OnceLock<GitListener> = OnceLock::new();

/// 깃 신호가 쌓이면 부를 자리 — GUI 의 Git 열 일꾼을 깨운다. 앱이 한 번 건다.
pub fn set_git_listener(listener: impl Fn() + Send + Sync + 'static) {
    let _ = GIT_LISTENER.set(Arc::new(listener));
}

fn git_signal(surface: &str, event: &Value) -> GitSignal {
    let clean = |s: &str| (!s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control)).then(|| s.to_string());
    GitSignal {
        seq: 0,
        surface: surface.to_string(),
        cwd: clean(&text(event, "cwd")).unwrap_or_default(),
        paths: event
            .get("paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(clean)
            .take(GIT_PATH_CAP)
            .collect(),
    }
}

fn record_git(signals: Vec<GitSignal>) {
    {
        let mut store = GIT_SIGNALS.lock().unwrap();
        for mut signal in signals {
            store.seq += 1;
            signal.seq = store.seq;
            if store.recent.len() >= GIT_SIGNAL_CAP {
                store.recent.pop_front();
            }
            store.recent.push_back(signal);
        }
    }
    GIT_WAKE.notify_waiters();
    if let Some(listener) = GIT_LISTENER.get() {
        listener();
    }
}

pub fn git_seq() -> u64 {
    GIT_SIGNALS.lock().unwrap().seq
}

/// `seen` 뒤의 신호와 지금 번호. 고리에서 밀려난 신호가 있으면 `lost` — 무엇이 바뀌었는지 모르니 다시 읽을 때다.
pub fn git_signals_since(seen: u64) -> (u64, Vec<GitSignal>, bool) {
    let store = GIT_SIGNALS.lock().unwrap();
    let lost = store.recent.front().is_some_and(|first| first.seq > seen.saturating_add(1));
    let fresh = store.recent.iter().filter(|s| s.seq > seen).cloned().collect();
    (store.seq, fresh, lost)
}

/// 깃 신호 번호가 `seen` 을 넘거나 `wait` 가 지날 때까지 기다린다. 지금 번호를 돌려준다.
pub async fn wait_git(seen: u64, wait: Duration) -> u64 {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = GIT_WAKE.notified();
        let seq = git_seq();
        if seq > seen || tokio::time::Instant::now() >= deadline {
            return seq;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return git_seq();
        }
    }
}

/// 이 신호가 이 칸 또는 이 저장소(`root`)를 건드렸을 수 있나. 고친 파일이 있으면 그 경로로, 명령이면 그 claude 의
/// 폴더로 가른다 — 같은 작업 트리를 함께 쓰는 다른 칸의 claude 가 고친 것도 이 칸의 Git 열에 바로 닿는다.
pub fn git_signal_touches(signal: &GitSignal, surface: &str, root: &std::path::Path) -> bool {
    if signal.surface == surface {
        return true;
    }
    let cwd = std::path::Path::new(&signal.cwd);
    if signal.paths.is_empty() {
        return !signal.cwd.is_empty() && cwd.starts_with(root);
    }
    signal.paths.iter().any(|path| {
        let path = std::path::Path::new(path);
        if path.is_absolute() { path.starts_with(root) } else { !signal.cwd.is_empty() && cwd.join(path).starts_with(root) }
    })
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
    bump(surface);
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
    bump(surface);
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

// ── 거울 대화 입력 받은편지함 ───────────────────────────────────────────────
// tell·done 은 여기를 거치지 않는다 — 모든 claude 칸에 입력칸 붙여넣기+Enter 로 넣는다(`tell_delivery.rs`).
// mod 의 `$.prompt.submit` 은 엔진이 쉰 뒤에야 돌려 일하는 칸에 턴 내내 묶였고, 받는 쪽 대화에
// 「The kasaterm-bridge plugin sent a message」 머리가 붙었다(2026-10-06 걷음).

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

fn offer(surface: &str, session: &str, id: &str, body: &str) {
    let mut inbox = INBOX.lock().unwrap();
    let letters = inbox.entry(surface.to_string()).or_default();
    if !letters.iter().any(|l| l.id == id) {
        letters.push(Letter { id: id.into(), session: session.into(), body: body.into(), offered: Instant::now(), taken: None });
    }
    drop(inbox);
    INBOX_WAKE.notify_waiters();
}

/// 거울(다른 기기·폰)의 대화 입력. 쉬는 순간 mod 가 정식 턴으로 넣고, 영수증 장부는 없다.
const CHAT_PREFIX: &str = "chat.";
/// 거울 입력이 학생이 쉬기를 기다리는 한도 — 긴 턴 뒤에도 들어가게 길게 둔다.
const CHAT_UNTAKEN: Duration = Duration::from_secs(1800);

fn is_chat(id: &str) -> bool {
    id.starts_with(CHAT_PREFIX)
}

/// 거울의 대화 입력을 이 칸 mod 에 맡긴다. mod 칸이 아니면 None — 부른 쪽이 다른 길로 넣는다.
pub fn offer_chat(surface: &str, body: &str) -> Option<String> {
    let state = live(surface)?;
    let id = format!("{CHAT_PREFIX}{}", uuid::Uuid::new_v4().simple());
    offer(surface, &state.session, &id, body);
    Some(id)
}

/// 오래된 거울 입력을 버린다 — 쉬기를 30분 넘게 못 기다린 것, 꺼내 갔는데 답이 없는 것. 앱의 tell 틱이 부른다.
pub fn sweep_inbox() {
    let mut inbox = INBOX.lock().unwrap();
    for letters in inbox.values_mut() {
        letters.retain(|l| match l.taken {
            None => l.offered.elapsed() < CHAT_UNTAKEN,
            Some(at) => at.elapsed() < OFFER_UNACKED,
        });
    }
    inbox.retain(|_, l| !l.is_empty());
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

/// mod 가 넣었다(또는 엔진이 거절했다). 대화 보기를 깨운다.
pub fn ack(surface: &str, session: &str, id: &str) -> Result<(), &'static str> {
    {
        let mut inbox = INBOX.lock().unwrap();
        let letters = inbox.get_mut(surface).ok_or("unknown")?;
        let at = letters.iter().position(|l| l.id == id && l.session == session).ok_or("unknown")?;
        letters.remove(at);
    }
    bump(surface);
    Ok(())
}

// ── 보낸 칸 알림 ───────────────────────────────────────────────────────────

/// 보낸 칸에 띄울 한 줄이 mod 를 기다리는 한도. 그동안 안 가져가면(mod 가 죽음·옛 판) 버린다 — 정본은 영수증이다.
const NOTICE_TTL: Duration = Duration::from_secs(600);
const NOTICE_CAP: usize = 20;

static NOTICES: LazyLock<Mutex<HashMap<String, Vec<(String, Instant)>>>> = LazyLock::new(Default::default);
static NOTICES_WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// 그 칸 mod 에게 토스트 한 줄을 맡긴다. 프롬프트로 넣으면 그 칸의 턴을 깨운다 — 토스트는 대화에도 모델에도
/// 안 들어간다. mod 칸이 아니면 띄울 자리가 없어 맡기지 않는다.
pub fn notice(surface: &str, text: &str) -> bool {
    if live(surface).is_none() {
        return false;
    }
    push_notice(surface, text);
    true
}

fn push_notice(surface: &str, text: &str) {
    let mut notices = NOTICES.lock().unwrap();
    let list = notices.entry(surface.to_string()).or_default();
    list.retain(|(_, at)| at.elapsed() < NOTICE_TTL);
    if list.len() >= NOTICE_CAP {
        list.remove(0);
    }
    list.push((text.to_string(), Instant::now()));
    drop(notices);
    NOTICES_WAKE.notify_waiters();
}

/// mod 의 긴 폴링 — 그 칸에 맡긴 한 줄을 꺼내 간다. 토스트는 턴과 상관없어 일하는 중에도 연다.
pub async fn take_notices(surface: &str, hold: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + hold;
    loop {
        let notified = NOTICES_WAKE.notified();
        let taken: Vec<String> = NOTICES
            .lock()
            .unwrap()
            .remove(surface)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, at)| at.elapsed() < NOTICE_TTL)
            .map(|(text, _)| text)
            .collect();
        if !taken.is_empty() {
            return taken;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return Vec::new();
        }
    }
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

/// `/claude-mod/*` 경로와 거울 창구(`/term/mod-*`). http.rs 의 큰 라우터에 그대로 붙는다.
///
/// `/claude-mod/*` 는 loopback 전용(이 기계의 mod·앱)이고, `/term/mod-*` 는 거울이 원본에 묻는 길이라 다른
/// `/term/*` 처럼 서버 관문(원격이면 토큰)만 탄다 — 그 거울은 이미 `/send` 로 이 칸에 무엇이든 칠 수 있으니
/// 승인 결정을 열어도 새 권한이 아니다.
pub fn routes() -> Router {
    Router::new()
        .route(
            "/term/mod-live",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let surface = q.get("surface").cloned().unwrap_or_default();
                if let Some(seen) = q.get("seq").and_then(|s| s.parse::<u64>().ok()) {
                    wait_seq(&surface, seen, hold_of(&q)).await;
                }
                Json(mirror_view(&surface)).into_response()
            }),
        )
        .route(
            "/term/mod-decide",
            post(|Json(body): Json<Value>| async move {
                let result = decide(
                    &text(&body, "id"),
                    &text(&body, "surface"),
                    &text(&body, "session"),
                    &text(&body, "decision"),
                    &text(&body, "message"),
                    &format!("mirror:{}", text(&body, "by")),
                );
                Json(match result {
                    Ok(()) => json!({"ok": true}),
                    Err(error) => json!({"ok": false, "error": error}),
                })
                .into_response()
            }),
        )
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
            "/claude-mod/notices",
            get(|Query(q): Query<HashMap<String, String>>, req: HttpRequest| async move {
                if let Err(r) = local_body(req).await {
                    return r;
                }
                let surface = q.get("surface").cloned().unwrap_or_default();
                Json(json!({"notices": take_notices(&surface, hold_of(&q)).await})).into_response()
            }),
        )
        .route(
            "/claude-mod/inbox/ack",
            post(|req: HttpRequest| async move {
                let body = match local_body(req).await {
                    Ok(body) => body,
                    Err(r) => return r,
                };
                let result = ack(&text(&body, "surface"), &text(&body, "session"), &text(&body, "id"));
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
    fn a_status_line_is_drawn_until_the_engine_redraws_its_own() {
        apply("%st", "s", &ev(json!({"kind": "status", "line": "early"})));
        assert_eq!(status_overlay("%st", "old"), None, "hello 없이 온 줄은 버린다");
        apply("%st", "s", &ev(json!({"kind": "hello", "pid": 1})));
        apply("%st", "other", &ev(json!({"kind": "status", "line": "stranger"})));
        assert_eq!(status_overlay("%st", "old"), None, "다른 세션의 줄은 안 받는다");

        apply("%st", "s", &ev(json!({"kind": "status", "line": "Sonnet 5.5"})));
        assert_eq!(status_overlay("%st", "Opus 5.5").as_deref(), Some("Sonnet 5.5"));
        assert_eq!(status_overlay("%st", "Opus 5.5").as_deref(), Some("Sonnet 5.5"), "엔진이 그대로면 계속 덧그린다");
        assert_eq!(status_overlay("%st", "Sonnet 5.5"), None, "엔진이 다시 그렸으면 손을 뗀다");
        assert_eq!(status_overlay("%st", "Opus 5.5"), None, "한 번 뗀 줄은 다시 안 쥔다");

        apply("%st", "s", &ev(json!({"kind": "status", "line": "low"})));
        assert_eq!(status_overlay("%st", "xhigh").as_deref(), Some("low"), "새 줄은 다시 쥔다");
        let Some(long_ago) = Instant::now().checked_sub(STATUS_HOLD + Duration::from_millis(1)) else { return };
        PANES.lock().unwrap().get_mut("%st").unwrap().status.as_mut().unwrap().at = long_ago;
        assert_eq!(status_overlay("%st", "xhigh"), None, "엔진이 멈춰도 한도가 지나면 엔진 줄로 돌아간다");
    }

    #[test]
    fn the_mirror_view_carries_open_tools_and_wakes_on_every_change() {
        apply("%t9", "s", &ev(json!({"kind": "hello", "pid": 1})));
        let before = seq("%t9");
        apply("%t9", "s", &ev(json!({"kind": "tool", "phase": "start", "id": "a", "tool": "Bash", "label": "cargo test"})));
        assert!(seq("%t9") > before);
        assert_eq!(mirror_view("%t9")["live"], false, "no claude runs in this test pane");
        apply("%t9", "s", &[json!({"kind": "turn", "phase": "start"}), json!({"kind": "turn", "phase": "end", "reason": "aborted"})]);
        assert!(PANES.lock().unwrap()["%t9"].focus.tools.is_empty(), "an aborted turn leaves no tool spinning");
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
    async fn a_git_signal_wakes_the_reader_and_names_only_its_pane_folder_and_paths() {
        apply("%t20", "s", &ev(json!({"kind": "hello", "pid": 1})));
        let seen = git_seq();
        let wait = tokio::spawn(async move { wait_git(seen, Duration::from_secs(5)).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        apply("%t20", "other", &ev(json!({"kind": "git", "tool": "Edit", "paths": ["/repo/x.rs"], "cwd": "/repo"})));
        apply("%t20", "s", &ev(json!({"kind": "git", "tool": "Edit", "paths": ["/repo/a.rs", "bad\npath"], "cwd": "/repo"})));
        assert!(tokio::time::timeout(Duration::from_secs(1), wait).await.unwrap().unwrap() > seen);
        let (_, signals, _) = git_signals_since(seen);
        let mine: Vec<_> = signals.iter().filter(|s| s.surface == "%t20").collect();
        assert_eq!(mine.len(), 1, "a signal from another session is dropped like any other event");
        assert_eq!(mine[0].paths, vec!["/repo/a.rs".to_string()]);
        assert!(!PANES.lock().unwrap()["%t20"].activity.iter().any(|row| row["kind"] == "git"));
    }

    #[test]
    fn the_focus_card_follows_tools_and_background_and_every_change_bumps_the_pane() {
        apply("%f1", "s", &ev(json!({"kind": "hello", "pid": 1})));
        let before = seq("%f1");
        apply("%f1", "s", &ev(json!({"kind": "turn", "phase": "start", "turn": "t"})));
        apply("%f1", "s", &ev(json!({"kind": "tool", "phase": "start", "id": "a", "tool": "Bash", "label": "npm test"})));
        apply("%f1", "s", &ev(json!({"kind": "tool", "phase": "start", "id": "b", "tool": "Read", "label": "main.rs", "agent": "sub1"})));
        apply("%f1", "s", &ev(json!({"kind": "tool", "phase": "end", "id": "b", "tool": "Read", "error": true})));
        apply("%f1", "s", &ev(json!({"kind": "background", "tasks": [{"id": "bg1", "type": "shell", "status": "running", "label": "npm run dev"}]})));
        assert!(seq("%f1") >= before + 5, "도구 하나가 시작·끝나도 판이 오른다");
        let state = PANES.lock().unwrap()["%f1"].state.clone();
        let facts = facts_of("%f1", &state).unwrap();
        assert_eq!(facts.state, "working");
        assert_eq!(facts.tools.iter().map(|t| t.tool.as_str()).collect::<Vec<_>>(), ["Bash"]);
        assert_eq!(facts.recent.len(), 1);
        assert!(facts.recent[0].agent && facts.recent[0].error && facts.recent[0].took_ms.is_some());
        assert_eq!(facts.background[0].label, "npm run dev");
        apply("%f1", "s", &ev(json!({"kind": "background", "tasks": []})));
        apply("%f1", "s", &ev(json!({"kind": "turn", "phase": "end", "turn": "t"})));
        let state = PANES.lock().unwrap()["%f1"].state.clone();
        let facts = facts_of("%f1", &state).unwrap();
        assert_eq!(facts.state, "resting");
        assert!(facts.tools.is_empty() && facts.background.is_empty(), "턴이 끝나면 못 받은 끝남도 거둔다");
        assert!(PANES.lock().unwrap()["%f1"].focus.task_since.is_empty());
    }

    #[test]
    fn only_the_last_few_finished_tools_are_kept_newest_first() {
        apply("%f2", "s", &ev(json!({"kind": "hello", "pid": 1})));
        for i in 0..8 {
            let id = format!("t{i}");
            apply("%f2", "s", &ev(json!({"kind": "tool", "phase": "start", "id": id, "tool": "Edit", "label": format!("f{i}")})));
            apply("%f2", "s", &ev(json!({"kind": "tool", "phase": "end", "id": id, "tool": "Edit"})));
        }
        let state = PANES.lock().unwrap()["%f2"].state.clone();
        let recent = facts_of("%f2", &state).unwrap().recent;
        assert_eq!(recent.len(), RECENT_TOOLS);
        assert_eq!(recent[0].label, "f7");
    }

    #[test]
    fn a_git_signal_reaches_its_own_pane_and_any_pane_on_the_same_repository() {
        let root = std::path::Path::new("/work/repo");
        let edit = GitSignal { seq: 1, surface: "%1".into(), cwd: "/work/repo".into(), paths: vec!["/work/repo/src/a.rs".into()] };
        assert!(git_signal_touches(&edit, "%1", std::path::Path::new("/elsewhere")));
        assert!(git_signal_touches(&edit, "%2", root));
        assert!(!git_signal_touches(&edit, "%2", std::path::Path::new("/work/repo-other")));
        let relative = GitSignal { paths: vec!["src/a.rs".into()], ..edit.clone() };
        assert!(git_signal_touches(&relative, "%2", root));
        let outside = GitSignal { cwd: "/work/repo".into(), paths: vec!["/tmp/scratch.txt".into()], ..edit.clone() };
        assert!(!git_signal_touches(&outside, "%2", root), "an edit outside the tree leaves another pane's column alone");
        let command = GitSignal { paths: vec![], cwd: "/work/repo/sub".into(), ..edit.clone() };
        assert!(git_signal_touches(&command, "%2", root));
        let homeless = GitSignal { paths: vec![], cwd: String::new(), ..edit };
        assert!(!git_signal_touches(&homeless, "%2", root));
    }

    #[test]
    fn signals_pushed_out_of_the_ring_are_reported_as_lost() {
        let start = git_seq();
        record_git((0..GIT_SIGNAL_CAP + 3).map(|_| GitSignal { seq: 0, surface: "%r".into(), cwd: String::new(), paths: vec![] }).collect());
        let (seq, _, lost) = git_signals_since(start);
        assert!(lost);
        assert_eq!(git_signals_since(seq), (seq, vec![], false));
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
        offer("%t8", "s", "chat.a", "hello");
        assert!(take("%t8", "other", Duration::from_millis(10)).await.is_empty());
        let got = take("%t8", "s", Duration::from_millis(10)).await;
        assert_eq!(got, vec![json!({"id": "chat.a", "body": "hello"})]);
        assert!(take("%t8", "s", Duration::from_millis(10)).await.is_empty());
        assert_eq!(ack("%t8", "other", "chat.a"), Err("unknown"));
        assert_eq!(ack("%t8", "s", "chat.a"), Ok(()));
        assert_eq!(ack("%t8", "s", "chat.a"), Err("unknown"), "한 번만 끝맺는다");
    }

    #[tokio::test]
    async fn a_notice_wakes_the_waiting_mod_once_and_only_on_its_pane() {
        let waiting = tokio::spawn(take_notices("%t9", Duration::from_secs(5)));
        tokio::task::yield_now().await;
        push_notice("%t9", "쪽지 못 감");
        assert_eq!(waiting.await.unwrap(), vec!["쪽지 못 감".to_string()]);
        assert!(take_notices("%t9", Duration::from_millis(10)).await.is_empty());
        push_notice("%t10", "다른 칸");
        assert!(take_notices("%t9", Duration::from_millis(10)).await.is_empty());
        assert!(!notice("%t11", "mod 없는 칸"), "mod 칸이 아니면 맡기지 않는다");
    }

    #[test]
    fn oversized_input_is_cut_on_a_char_boundary() {
        let (input, cut) = cap_input(json!({"command": "가".repeat(INPUT_CAP)}));
        assert!(cut);
        assert!(input.as_str().unwrap().len() <= INPUT_CAP);
    }
}
