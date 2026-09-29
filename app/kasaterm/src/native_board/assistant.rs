use super::*;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub(crate) struct ChatSnapshot {
    pub(crate) draft: String,
    pub(crate) project_draft: String,
    pub(crate) project_editor: bool,
    pub(crate) context: Option<(String, String)>,
    pub(crate) error: Option<String>,
    pub(crate) loading: bool,
    pub(crate) online: bool,
    pub(crate) loaded: bool,
    pub(crate) authenticated: bool,
    pub(crate) busy: bool,
    pub(crate) key_editor: bool,
    pub(crate) key_mask: String,
    pub(crate) retryable: bool,
    pub(crate) workspace: Arc<Value>,
    pub(crate) scroll: f32,
    pub(crate) scroll_max: Arc<Mutex<f32>>,
    pub(crate) layout: Arc<Mutex<super::chat::Layout>>,
}

#[derive(Clone)]
struct Outgoing {
    id: String,
    text: String,
    task: Option<String>,
}

impl Outgoing {
    fn body(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({"id":self.id,"text":self.text,"task_id":self.task,"model":null}))
            .unwrap_or_default()
    }
}

enum Envelope {
    Read(
        Option<kasa_mcp::device_auth::Stamp>,
        Result<Value, &'static str>,
    ),
    Wrote(
        kasa_mcp::device_auth::Stamp,
        Option<String>,
        Result<(), &'static str>,
    ),
}

pub(crate) struct ChatState {
    pub(crate) draft: String,
    pub(crate) project_draft: String,
    project_editor: bool,
    key: String,
    key_editor: bool,
    context: Option<(String, String)>,
    workspace: Arc<Value>,
    account: Option<kasa_mcp::device_auth::Stamp>,
    error: Option<String>,
    last_refresh: Option<Instant>,
    observed_at: u64,
    reading: bool,
    writing: bool,
    loaded: bool,
    outgoing: Option<Outgoing>,
    retryable: bool,
    mailbox: Arc<Mutex<Vec<Envelope>>>,
    scroll: f32,
    scroll_max: Arc<Mutex<f32>>,
    layout: Arc<Mutex<super::chat::Layout>>,
    fixture: bool,
    /// (task, rev) pairs already handed to the server claim, so one tick never asks twice.
    claimed: std::collections::HashSet<(String, u64)>,
    /// request id → (state, step) last sent, so only a semantic change goes over the wire.
    observed: std::collections::HashMap<String, (String, String)>,
    last_observe: Option<Instant>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            project_draft: String::new(),
            project_editor: false,
            key: String::new(),
            key_editor: false,
            context: None,
            workspace: Arc::new(json!({})),
            account: None,
            error: None,
            last_refresh: None,
            observed_at: 0,
            reading: false,
            writing: false,
            loaded: false,
            outgoing: None,
            retryable: false,
            mailbox: Arc::new(Mutex::new(Vec::new())),
            scroll: 0.0,
            scroll_max: Arc::new(Mutex::new(0.0)),
            layout: Arc::new(Mutex::new(super::chat::Layout::default())),
            fixture: false,
            claimed: std::collections::HashSet::new(),
            observed: std::collections::HashMap::new(),
            last_observe: None,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}
pub(super) fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(super) fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
pub(super) fn key_present(value: &Value) -> bool {
    value["status"]["key_present"].as_bool() == Some(true)
}
pub(super) fn verified(task: &Value) -> bool {
    string(task, "state") == "verified" && task["verify_ok"].as_bool() == Some(true)
}
pub(super) fn phase(task: &Value) -> &'static str {
    match string(task, "state") {
        "working" => "진행 중",
        "blocked" => "답변 필요",
        "awaiting_verification" => "검증 중",
        "verified" if verified(task) => "검증 통과",
        "verified" => "검증 미확인",
        "cancelled" => "접음",
        _ => "접수됨",
    }
}

fn problem(code: &str) -> &'static str {
    match code {
        "signed_out" | "unauthorized" => "로그인하면 개인 작업과 대화를 이어 볼 수 있어요.",
        "key_required" => "OpenGateway 키를 등록해 주세요.",
        "revision_conflict" => "다른 기기에서 설정이 바뀌었어요. 새로고침 후 다시 등록해 주세요.",
        "model_setup" | "model_setup_required" => {
            "이 키에서 사용할 대화 모델을 찾지 못했어요. OpenGateway 모델 권한을 확인해 주세요."
        }
        "invalid_request" => "입력 내용을 확인해 주세요.",
        "rate_limited" => "요청이 많아 잠시 쉬고 있어요. 조금 뒤 다시 확인해 주세요.",
        "account_changed" => "계정이 바뀌어 이전 요청을 중단했어요.",
        "not_found" => "계정 서버 업데이트가 필요해요.",
        _ => "계정 서버에 연결하지 못했어요. 입력은 유지되며 같은 요청만 다시 확인해요.",
    }
}

/// A transport failure or an undecodable reply means the server may have applied the
/// write anyway. Such a request keeps its id so the only retry is the same request.
fn outcome_unknown(code: &str) -> bool {
    matches!(code, "unavailable" | "unreachable")
}

fn decoded(status: u16, bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("unavailable");
    }
    if status == 401 || status == 403 {
        return Err("unauthorized");
    }
    if status == 404 {
        return Err("not_found");
    }
    if status == 409 {
        return Err("revision_conflict");
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "unavailable")?;
    if !(200..300).contains(&status) {
        return Err(match value.get("error").and_then(Value::as_str) {
            Some("key_required") => "key_required",
            Some("invalid_request") => "invalid_request",
            Some("rate_limited") => "rate_limited",
            Some("model_setup" | "model_setup_required") => "model_setup",
            _ => "unavailable",
        });
    }
    Ok(value)
}

/// 받는 곳 판정 한 번 — 관문 `/route`. 부르는 쪽이 일꾼 스레드에서 부른다.
pub(super) fn route_blocking(body: Vec<u8>) -> Result<Value, &'static str> {
    let session = kasa_mcp::WorkspaceDesktopSession::capture().ok_or("signed_out")?;
    let result = session.request("POST", "/route", Some(&body)).and_then(|(status, bytes)| decoded(status, &bytes));
    if !session.is_current() {
        return Err("account_changed");
    }
    result
}

fn wipe(value: &mut String) {
    let mut bytes = std::mem::take(value).into_bytes();
    bytes.fill(0);
}

impl Drop for ChatState {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

impl ChatState {
    pub(crate) fn snapshot(&self) -> ChatSnapshot {
        ChatSnapshot {
            draft: self.draft.clone(),
            project_draft: self.project_draft.clone(),
            project_editor: self.project_editor,
            context: self.context.clone(),
            error: self.error.clone(),
            loading: self.reading,
            online: self.loaded && self.error.is_none(),
            loaded: self.loaded,
            authenticated: self.account.is_some() || self.fixture,
            busy: self.writing,
            key_editor: self.key_editor || !key_present(&self.workspace),
            key_mask: "*".repeat(self.key.chars().count()),
            retryable: self.retryable,
            workspace: self.workspace.clone(),
            scroll: self.scroll,
            scroll_max: self.scroll_max.clone(),
            layout: self.layout.clone(),
        }
    }
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn key(&self) -> &str {
        &self.key
    }
    pub(crate) fn key_mut(&mut self) -> &mut String {
        &mut self.key
    }
    pub(crate) fn toggle_key_editor(&mut self) {
        self.key_editor = !self.key_editor;
        if !self.key_editor {
            wipe(&mut self.key);
        }
    }
    pub(crate) fn toggle_project_editor(&mut self) {
        self.project_editor = !self.project_editor;
    }
    pub(crate) fn create_project(&mut self) {
        if self.fixture || self.writing || self.project_draft.trim().is_empty() {
            return;
        }
        let body = serde_json::to_vec(
            &json!({"name":self.project_draft.trim(),"kind":"general","keywords":[]}),
        )
        .unwrap_or_default();
        self.write("POST", "/projects", body, None);
    }
    pub(crate) fn set_task(&mut self, task: Option<(String, String)>) {
        self.context = task;
    }
    pub(crate) fn task_title(&self, id: &str) -> Option<String> {
        rows(&self.workspace, "tasks")
            .iter()
            .find(|task| string(task, "id") == id)
            .map(|task| string(task, "goal").to_string())
    }
    pub(crate) fn refresh_due(&self) -> bool {
        !self.fixture
            && !self.reading
            && self
                .last_refresh
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(5))
    }
    pub(crate) fn active(&self) -> bool {
        self.loaded || self.writing
    }
    pub(crate) fn refresh(&mut self) {
        if self.fixture || self.reading {
            return;
        }
        self.reading = true;
        self.last_refresh = Some(Instant::now());
        let mailbox = self.mailbox.clone();
        std::thread::spawn(move || {
            let event = if let Some(session) = kasa_mcp::WorkspaceDesktopSession::capture() {
                let stamp = session.stamp();
                let result = session
                    .request("GET", "/snapshot", None)
                    .and_then(|(status, body)| decoded(status, &body));
                if !session.is_current() {
                    return;
                }
                Envelope::Read(Some(stamp), result)
            } else {
                Envelope::Read(None, Err("signed_out"))
            };
            if let Ok(mut slot) = mailbox.lock() {
                slot.push(event);
            }
        });
    }
    pub(crate) fn register_key(&mut self) {
        if self.fixture || self.writing || self.account.is_none() {
            return;
        }
        if self.key.len() < 8
            || self.key.len() > 4096
            || !self.key.bytes().all(|byte| byte.is_ascii_graphic())
        {
            self.error = Some("키는 공백 없는 8~4096자여야 해요.".into());
            return;
        }
        let body=serde_json::to_vec(&json!({"expected_key_revision":number(&self.workspace["status"],"key_revision"),"key":self.key})).unwrap_or_default();
        wipe(&mut self.key);
        self.write("PUT", "/key", body, None);
    }
    pub(crate) fn send(&mut self) {
        if self.fixture
            || self.writing
            || self.outgoing.is_some()
            || !key_present(&self.workspace)
            || self.account.is_none()
        {
            return;
        }
        if self.draft.trim().is_empty() || self.draft.len() > 16384 {
            self.error = Some("요청은 비어 있지 않은 16KB 이하의 글로 적어 주세요.".into());
            return;
        }
        let outgoing = Outgoing {
            id: uuid::Uuid::new_v4().simple().to_string(),
            text: self.draft.clone(),
            task: self.context.as_ref().map(|(id, _)| id.clone()),
        };
        let body = outgoing.body();
        let id = outgoing.id.clone();
        self.outgoing = Some(outgoing);
        self.retryable = false;
        self.write("POST", "/messages", body, Some(id));
    }
    pub(crate) fn retry(&mut self) {
        if self.writing || self.fixture {
            return;
        }
        if let Some(out) = self.outgoing.as_ref().filter(|_| self.retryable) {
            let body = out.body();
            let id = out.id.clone();
            self.write("POST", "/messages", body, Some(id));
        } else {
            self.last_refresh = None;
            self.refresh();
        }
    }
    fn write(
        &mut self,
        method: &'static str,
        path: &'static str,
        mut body: Vec<u8>,
        id: Option<String>,
    ) {
        let Some(expected) = self.account.clone() else {
            body.fill(0);
            return;
        };
        self.writing = true;
        self.error = None;
        let mailbox = self.mailbox.clone();
        std::thread::spawn(move || {
            let result = if let Some(session) = kasa_mcp::WorkspaceDesktopSession::capture() {
                if session.stamp() != expected {
                    Err("account_changed")
                } else {
                    let result = session
                        .request(method, path, Some(&body))
                        .and_then(|(status, body)| decoded(status, &body))
                        .map(|_| ());
                    if session.is_current() {
                        result
                    } else {
                        Err("account_changed")
                    }
                }
            } else {
                Err("signed_out")
            };
            body.fill(0);
            if let Ok(mut slot) = mailbox.lock() {
                slot.push(Envelope::Wrote(expected, id, result));
            }
        });
    }
    pub(crate) fn pump(&mut self) -> bool {
        let events = self
            .mailbox
            .lock()
            .map(|mut slot| std::mem::take(&mut *slot))
            .unwrap_or_default();
        let changed = !events.is_empty();
        for event in events {
            match event {
                Envelope::Read(stamp, result) => {
                    self.reading = false;
                    if stamp.as_ref().is_some_and(|stamp| {
                        !kasa_mcp::device_auth::stamp_is_current(stamp)
                    }) {
                        continue;
                    }
                    self.account = stamp;
                    match result {
                        Ok(value) => {
                            let value = value.get("snapshot").cloned().unwrap_or(value);
                            if !value["status"].is_object()
                                || !value["tasks"].is_array()
                                || !value["projects"].is_array()
                            {
                                self.error = Some(problem("unavailable").into());
                                continue;
                            }
                            if number(&value["status"], "revision")
                                < number(&self.workspace["status"], "revision")
                            {
                                continue;
                            }
                            self.workspace = Arc::new(value);
                            self.loaded = true;
                            self.observed_at = now();
                            if !self.retryable {
                                self.error = None;
                            }
                            if self.outgoing.as_ref().is_some_and(|out| {
                                rows(&self.workspace, "conversation")
                                    .iter()
                                    .any(|row| string(row, "id") == out.id)
                            }) {
                                if let Some(out) = self.outgoing.take() {
                                    if self.draft == out.text {
                                        self.draft.clear();
                                    }
                                }
                                self.retryable = false;
                                self.error = None;
                            }
                        }
                        Err(code) => {
                            self.error = Some(problem(code).into());
                            if matches!(code, "signed_out" | "unauthorized") {
                                self.workspace = Arc::new(json!({}));
                                self.loaded = false;
                                wipe(&mut self.key);
                            }
                        }
                    }
                }
                Envelope::Wrote(stamp, id, result) => {
                    if self.account.as_ref() != Some(&stamp) {
                        continue;
                    }
                    self.writing = false;
                    self.last_refresh = None;
                    match result {
                        Ok(()) => {
                            if let Some(id) = id {
                                if self.outgoing.as_ref().is_some_and(|out| out.id == id) {
                                    let out = self.outgoing.take().unwrap();
                                    if self.draft == out.text {
                                        self.draft.clear();
                                    }
                                }
                            } else {
                                self.key_editor = false;
                                self.project_editor = false;
                                self.project_draft.clear();
                            }
                            self.retryable = false;
                            self.error = None;
                        }
                        Err(code) => {
                            if id.is_some() && self.outgoing.is_none() {
                                self.retryable = false;
                                self.error = None;
                            } else {
                                self.retryable = id.is_some() && outcome_unknown(code);
                                if !self.retryable && id.is_some() {
                                    self.outgoing = None;
                                }
                                self.error = Some(problem(code).into());
                            }
                        }
                    }
                }
            }
        }
        changed
    }
    pub(crate) fn scroll_by(&mut self, delta: f32) -> bool {
        let max = self.scroll_max.lock().map(|v| *v).unwrap_or(0.0);
        let next = (self.scroll + delta).clamp(0.0, max);
        let changed = next != self.scroll;
        self.scroll = next;
        changed
    }
    pub(crate) fn pet_status(&self) -> Value {
        let current = self
            .account
            .as_ref()
            .is_some_and(|stamp| kasa_mcp::device_auth::stamp_is_current(stamp));
        let fresh = current
            && self.loaded
            && self.observed_at.saturating_add(90) >= now()
            && self.error.is_none();
        let mut tasks = rows(&self.workspace, "tasks").iter().collect::<Vec<_>>();
        tasks.sort_by_key(|task| match string(task, "state") {
            "blocked" => 0,
            "working" => 1,
            "awaiting_verification" => 2,
            "queued" => 3,
            _ => 4,
        });
        let task = tasks.into_iter().find(|task| {
            matches!(
                string(task, "state"),
                "blocked" | "working" | "awaiting_verification" | "queued"
            )
        });
        let configured = fresh && key_present(&self.workspace);
        let state = if !configured {
            "idle"
        } else {
            match task.map(|task| string(task, "state")) {
                Some("blocked") => "wait",
                Some(_) => "busy",
                None => "idle",
            }
        };
        let text = if configured {
            task.map(|task| format!("{} · {}", string(task, "goal"), phase(task)))
                .unwrap_or_default()
        } else {
            String::new()
        };
        json!({"schema":"kasa.workspace.pet.v1","revision":number(&self.workspace["status"],"revision"),"observed_at":self.observed_at,
            "account_current":current,"key_present":configured,"state":state,"text":text,"pane":"","focus":null,
            "task_id":task.map(|task|string(task,"id")),"verified":false})
    }
    pub(crate) fn notification_candidates(&self) -> Vec<(String, u64)> {
        if self.fixture
            || !key_present(&self.workspace)
            || self.error.is_some()
            || !self.loaded
            || self.observed_at.saturating_add(90) < now()
            || !self
                .account
                .as_ref()
                .is_some_and(|stamp| kasa_mcp::device_auth::stamp_is_current(stamp))
        {
            return Vec::new();
        }
        rows(&self.workspace, "tasks")
            .iter()
            .filter(|task| verified(task))
            .map(|task| (string(task, "id").into(), number(task, "rev")))
            .collect()
    }
    /// Completion notifications: only the server can hand one out, and only for a task whose
    /// trusted evidence passed. The claim is per (task, rev) across every device of the account,
    /// so the same completion never rings twice. Nothing is claimed where nothing can ring.
    pub(crate) fn claim_completions(&mut self) {
        if self.fixture || crate::lite_mode() {
            return;
        }
        if self.claimed.len() > 512 {
            self.claimed.clear();
        }
        let Some(expected) = self.account.clone() else {
            return;
        };
        for (task, rev) in self.notification_candidates() {
            if !self.claimed.insert((task.clone(), rev)) {
                continue;
            }
            let expected = expected.clone();
            std::thread::spawn(move || {
                let Some(session) = kasa_mcp::WorkspaceDesktopSession::capture() else {
                    return;
                };
                if session.stamp() != expected {
                    return;
                }
                let body = serde_json::to_vec(&json!({"task_id":task,"revision":rev})).unwrap_or_default();
                let Ok(value) = session
                    .request("POST", "/notifications/claim", Some(&body))
                    .and_then(|(status, bytes)| decoded(status, &bytes))
                else {
                    return;
                };
                if !session.is_current() {
                    return;
                }
                let notification = &value["notification"];
                if !notification.is_object() {
                    return;
                }
                let goal = string(notification, "goal");
                crate::chrome::notify_desktop(
                    "작업 완료 · 검사 통과",
                    if goal.is_empty() { "검사 근거가 확인된 작업이 끝났어요." } else { goal },
                    None,
                    Some(&format!("workspace-done:{task}:{rev}")),
                    None,
                );
            });
        }
    }
    /// Observation window: a live account with a key, at most once per `every`.
    pub(crate) fn observe_ready(&mut self, every: Duration) -> Option<kasa_mcp::device_auth::Stamp> {
        if self.fixture || !self.loaded || self.error.is_some() || !key_present(&self.workspace) {
            return None;
        }
        let stamp = self.account.clone()?;
        if !kasa_mcp::device_auth::stamp_is_current(&stamp)
            || self.last_observe.is_some_and(|at| at.elapsed() < every)
        {
            return None;
        }
        self.last_observe = Some(Instant::now());
        Some(stamp)
    }
    pub(crate) fn observe_known(&self, request_id: &str) -> bool {
        self.observed.contains_key(request_id)
    }
    /// Records the observation and says whether it differs from the last one sent.
    pub(crate) fn observe_changed(&mut self, found: &super::observe::Observation) -> bool {
        if self.observed.len() > 512 {
            self.observed.clear();
        }
        let next = (found.state.to_string(), found.step.clone());
        if self.observed.get(&found.request_id) == Some(&next) {
            return false;
        }
        self.observed.insert(found.request_id.clone(), next);
        true
    }
    /// One idempotent task upsert; the account must still be the one that was observed.
    pub(crate) fn observe_send(&self, expected: kasa_mcp::device_auth::Stamp, mut body: Vec<u8>) {
        std::thread::spawn(move || {
            if let Some(session) = kasa_mcp::WorkspaceDesktopSession::capture() {
                if session.stamp() == expected {
                    let _ = session.request("POST", "/tasks", Some(&body));
                }
            }
            body.fill(0);
        });
    }
    pub(crate) fn set_fixture(&mut self) {
        self.reset();
        self.fixture = true;
        self.loaded = true;
        self.observed_at = now();
        self.workspace = Arc::new(
            json!({"status":{"enabled":true,"key_present":true,"key_revision":1,"revision":7},
            "projects":[{"id":"kasa","name":"KASA","kind":"coding"},{"id":"notes","name":"문서","kind":"research"}],
            "tasks":[
                {"id":"fixture-git","original_prompt":"현재 브랜치와 원격 브랜치를 보기 좋게 정리하고 실제 커밋 연결도 보여 주세요.","goal":"브랜치와 커밋 그래프","state":"working","project":"kasa","step":"병합과 경계 커밋 검사","rev":2,"work_revision":"fixture","required_checks":["graph-tests"],"verify_ok":false},
                {"id":"fixture-theme","original_prompt":"상단바와 pane 색을 따로 바꾸고 싶어요.","goal":"색 설정 간소화","state":"awaiting_verification","project":"kasa","step":"좁은 화면과 색 저장 확인","rev":3,"work_revision":"fixture","required_checks":["layout-tests"],"verify_ok":false},
                {"id":"fixture-note","original_prompt":"문서 열기 오류를 고쳐 주세요.","goal":"문서 열기 수정","state":"verified","project":"notes","step":"문서 열기 검사 통과","rev":4,"work_revision":"fixture","required_checks":["open-test"],"verify_ok":true}],
            "conversation":[{"id":"fixture-message","prompt":"지금 무엇을 확인해야 하나요?","reply":"브랜치 그래프와 색 설정은 검사 중입니다. 문서 열기는 검증을 통과했습니다.","status":"replied","model":"fixture","task_id":null}]}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_write_outcomes_keep_the_request_for_a_same_id_retry() {
        assert!(outcome_unknown("unreachable"));
        assert!(outcome_unknown("unavailable"));
        for settled in ["invalid_request", "key_required", "rate_limited", "unauthorized", "account_changed"] {
            assert!(!outcome_unknown(settled), "{settled} is a settled answer, not a retry case");
        }
    }
    #[test]
    fn keys_never_enter_render_snapshots_or_outgoing_conversation() {
        let mut state = ChatState::default();
        state.key = "private-key-never-render".into();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.key_mask, "*".repeat(24));
        assert!(snapshot.draft.is_empty());
        assert!(!snapshot.workspace.to_string().contains("private-key"));
        state.reset();
        assert!(state.key.is_empty());
        assert!(!state.loaded);
    }
    #[test]
    fn completion_uses_verified_server_state_and_not_freeform_text() {
        assert!(!verified(
            &json!({"state":"working","step":"done verified","verify_ok":true})
        ));
        assert!(!verified(&json!({"state":"verified","verify_ok":false})));
        assert!(verified(&json!({"state":"verified","verify_ok":true})));
    }
    #[test]
    fn retry_payload_keeps_its_original_identity_and_text() {
        let outgoing = Outgoing {
            id: "same-id".into(),
            text: "원문\n다음 줄".into(),
            task: Some("task".into()),
        };
        let decoded: Value = serde_json::from_slice(&outgoing.body()).unwrap();
        assert_eq!(decoded["id"], "same-id");
        assert_eq!(decoded["text"], "원문\n다음 줄");
        assert!(decoded["model"].is_null());
    }

    #[test]
    fn account_reset_disconnects_inflight_mailboxes_and_completion_candidates() {
        let mut state = ChatState::default();
        let previous = state.mailbox.clone();
        state.set_fixture();
        assert!(state.notification_candidates().is_empty());
        previous.lock().unwrap().push(Envelope::Read(
            None,
            Ok(json!({"status":{"revision":99},"tasks":[],"projects":[]})),
        ));
        assert!(!state.pump());
        assert_eq!(state.workspace["status"]["revision"], 7);
        state.reset();
        assert!(!state.loaded);
        assert!(state.snapshot().draft.is_empty());
    }
}
