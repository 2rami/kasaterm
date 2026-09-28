use super::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

const RETAIN_EVENTS: usize = 200;
const RETAIN_BYTES: usize = 512 * 1024;
const PAGE_ROWS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Event {
    seq: u64,
    kind: String,
    id: String,
    message: String,
    text: String,
    state: String,
    surface: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Outgoing {
    id: String,
    text: String,
    task: Option<(String, String)>,
    state: String,
    retryable: bool,
}

impl Outgoing {
    fn body(&self) -> Value {
        let mut body = json!({"id": self.id, "text": self.text});
        if let Some((task, rev)) = &self.task {
            body["task"] = Value::String(task.clone());
            body["rev"] = Value::String(rev.clone());
        }
        body
    }
}

struct Page {
    events: Vec<Event>,
    more: bool,
}

enum Delivery {
    Receipt(String),
    Uncertain(String),
    Rejected(String),
    AccountChanged,
    AuthorizationLost,
}

enum Envelope {
    Read(
        u64,
        Option<kasa_mcp::device_auth::Stamp>,
        Result<Page, String>,
    ),
    Sent(u64, String, Delivery),
    AuthorizationLost(u64),
}

#[derive(Default)]
struct LayoutCache {
    key: (u64, u32, u32),
    rows: Vec<PaintRow>,
    height: f32,
    scroll_max: f32,
}

#[derive(Clone)]
struct PaintRow {
    y: f32,
    label: String,
    lines: Vec<String>,
    own: bool,
    muted: bool,
}

#[derive(Clone)]
pub(crate) struct ChatSnapshot {
    pub(crate) draft: String,
    pub(crate) context: Option<(String, String)>,
    pub(crate) error: Option<String>,
    pub(crate) loading: bool,
    pub(crate) online: bool,
    pub(crate) loaded: bool,
    pub(crate) scroll: f32,
    pub(crate) authenticated: bool,
    events: Arc<Vec<Event>>,
    outgoing: Option<Outgoing>,
    rejected: Option<Outgoing>,
    revision: u64,
    layout: Arc<Mutex<LayoutCache>>,
}

pub(crate) struct ChatState {
    pub(crate) draft: String,
    context: Option<(String, String)>,
    events: Arc<Vec<Event>>,
    outgoing: Option<Outgoing>,
    rejected: Option<Outgoing>,
    account: Option<kasa_mcp::device_auth::Stamp>,
    cursor: u64,
    loaded: bool,
    online: bool,
    error: Option<String>,
    send_error: Option<String>,
    reading: bool,
    writing: bool,
    more: bool,
    last_refresh: Option<Instant>,
    generation: u64,
    revision: u64,
    mailbox: Arc<Mutex<Vec<Envelope>>>,
    layout: Arc<Mutex<LayoutCache>>,
    scroll: f32,
    fixture: bool,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            draft: String::new(),
            context: None,
            events: Arc::new(Vec::new()),
            outgoing: None,
            rejected: None,
            account: None,
            cursor: 0,
            loaded: false,
            online: false,
            error: None,
            send_error: None,
            reading: false,
            writing: false,
            more: false,
            last_refresh: None,
            generation: 0,
            revision: 1,
            mailbox: Arc::new(Mutex::new(Vec::new())),
            layout: Arc::new(Mutex::new(LayoutCache::default())),
            scroll: 0.0,
            fixture: false,
        }
    }
}

impl ChatState {
    pub(crate) fn snapshot(&self) -> ChatSnapshot {
        ChatSnapshot {
            draft: self.draft.clone(),
            context: self.context.clone(),
            error: self.send_error.clone().or_else(|| self.error.clone()),
            loading: self.reading,
            online: self.online,
            loaded: self.loaded,
            scroll: self.scroll,
            authenticated: self.account.is_some() || self.fixture,
            events: Arc::clone(&self.events),
            outgoing: self.outgoing.clone(),
            rejected: self.rejected.clone(),
            revision: self.revision,
            layout: Arc::clone(&self.layout),
        }
    }

    pub(crate) fn reset(&mut self) {
        // Workers keep the old mailbox, so an account change cannot repopulate the new conversation.
        let generation = self.generation.wrapping_add(1);
        *self = Self::default();
        self.generation = generation;
    }

    pub(crate) fn set_fixture(&mut self) {
        self.reset();
        self.fixture = true;
        self.events = Arc::new(vec![
            Event { seq: 1, kind: "message".into(), id: "fixture-1".into(), message: String::new(), text: "끝난 일과 제가 확인할 것만 정리해 줘.".into(), state: String::new(), surface: "app".into() },
            Event { seq: 2, kind: "reply".into(), id: String::new(), message: "fixture-1".into(), text: "문서 열기 수정은 확인됐어요.\n\n기기 연결 작업은 검증 중이고, 화면 정리는 방향 확인이 필요해요. 왼쪽에서 작업을 고르면 근거를 함께 볼 수 있어요.".into(), state: String::new(), surface: "app".into() },
            Event { seq: 3, kind: "status".into(), id: String::new(), message: "fixture-1".into(), text: String::new(), state: "answered".into(), surface: "app".into() },
        ]);
        self.online = true;
        self.loaded = true;
        self.cursor = 3;
    }

    pub(crate) fn set_task(&mut self, task: Option<(String, String)>) {
        self.context = task;
    }

    pub(crate) fn refresh_due(&self) -> bool {
        !self.fixture
            && !self.reading
            && (self.more
                || self.last_refresh.is_none_or(|at| {
                    at.elapsed() >= Duration::from_secs(if self.online { 2 } else { 5 })
                }))
    }

    pub(crate) fn refresh(&mut self) {
        if self.fixture || self.reading {
            return;
        }
        self.reading = true;
        self.last_refresh = Some(Instant::now());
        let path = if self.loaded {
            format!(
                "/api/app/events?after={}&limit={PAGE_ROWS}&wait=0",
                self.cursor
            )
        } else {
            format!("/api/app/events?tail={RETAIN_EVENTS}&wait=0")
        };
        let generation = self.generation;
        let expected = self.account.clone();
        let mailbox = Arc::clone(&self.mailbox);
        std::thread::spawn(move || {
            let (stamp, page) = if let Some(session) = kasa_mcp::NachoDesktopSession::capture() {
                let stamp = session.stamp();
                let path = if expected.as_ref() == Some(&stamp) {
                    path
                } else {
                    format!("/api/app/events?tail={RETAIN_EVENTS}&wait=0")
                };
                let response = session.request("GET", &path, None);
                if matches!(&response, Ok((status, _)) if authorization_lost(*status)) {
                    if let Ok(mut slot) = mailbox.lock() {
                        slot.push(Envelope::AuthorizationLost(generation));
                    }
                    return;
                }
                let page = response
                    .map_err(transport_problem)
                    .and_then(|(status, body)| parse_page(status, &body));
                if session.is_current() {
                    (Some(stamp), page)
                } else {
                    (None, Err(transport_problem("account_changed")))
                }
            } else {
                (None, Err(transport_problem("login_required")))
            };
            if let Ok(mut slot) = mailbox.lock() {
                slot.push(Envelope::Read(generation, stamp, page));
            }
        });
    }

    pub(crate) fn send(&mut self) {
        if self.fixture || self.writing || self.outgoing.is_some() || self.draft.trim().is_empty() {
            return;
        }
        if self.account.is_none() || !self.online {
            self.error = Some("계정 연결을 확인한 뒤 보낼 수 있어요.".into());
            return;
        }
        if self.draft.len() > 16 * 1024 {
            self.error = Some("메시지가 너무 길어요. 나누어서 보내 주세요.".into());
            return;
        }
        self.outgoing = Some(Outgoing {
            id: uuid::Uuid::new_v4().simple().to_string(),
            text: std::mem::take(&mut self.draft),
            task: self.context.clone(),
            state: "sending".into(),
            retryable: false,
        });
        self.scroll = 0.0;
        self.dispatch();
    }

    pub(crate) fn retry(&mut self) {
        if self.fixture || self.writing || !self.outgoing.as_ref().is_some_and(|out| out.retryable)
        {
            return;
        }
        self.dispatch();
    }

    fn dispatch(&mut self) {
        let Some(out) = &mut self.outgoing else {
            return;
        };
        out.state = "sending".into();
        out.retryable = false;
        self.writing = true;
        self.error = None;
        self.send_error = None;
        self.revision += 1;
        let body = out.body().to_string().into_bytes();
        let id = out.id.clone();
        let generation = self.generation;
        let expected = self.account.clone();
        let mailbox = Arc::clone(&self.mailbox);
        std::thread::spawn(move || {
            let result = match kasa_mcp::NachoDesktopSession::capture() {
                Some(session) if Some(session.stamp()) == expected => {
                    let result = match session.request("POST", "/api/app/messages", Some(&body)) {
                        Ok((status, _)) if authorization_lost(status) => {
                            Delivery::AuthorizationLost
                        }
                        Ok((status, bytes)) => parse_receipt(status, &bytes),
                        Err("invalid_request") => Delivery::Rejected(
                            "메시지가 너무 길거나 형식이 달라요. 내용을 줄인 뒤 다시 보내 주세요."
                                .into(),
                        ),
                        Err(_) => Delivery::Uncertain(
                            "접수 여부를 확인하지 못했어요. 같은 메시지로 다시 확인할 수 있어요."
                                .into(),
                        ),
                    };
                    if session.is_current() {
                        result
                    } else {
                        Delivery::AccountChanged
                    }
                }
                _ => Delivery::AccountChanged,
            };
            if let Ok(mut slot) = mailbox.lock() {
                slot.push(Envelope::Sent(generation, id, result));
            }
        });
    }

    pub(crate) fn pump(&mut self) -> bool {
        let envelopes = self
            .mailbox
            .lock()
            .map(|mut slot| std::mem::take(&mut *slot))
            .unwrap_or_default();
        if envelopes.is_empty() {
            return false;
        }
        let previous_events = Arc::clone(&self.events);
        let previous_outgoing = self.outgoing.clone();
        let previous_rejected = self.rejected.clone();
        let mut changed = false;
        for envelope in envelopes {
            match envelope {
                Envelope::AuthorizationLost(generation) if generation == self.generation => {
                    self.reset();
                    self.error = Some(transport_problem("authorization_lost"));
                    changed = true;
                }
                Envelope::Read(generation, account, result) if generation == self.generation => {
                    if self.account != account {
                        if self.account.is_some() {
                            self.events = Arc::new(Vec::new());
                            self.outgoing = None;
                            self.rejected = None;
                            self.send_error = None;
                            self.draft.clear();
                            self.context = None;
                            self.cursor = 0;
                            self.loaded = false;
                            self.writing = false;
                            self.scroll = 0.0;
                            self.generation += 1;
                        }
                        self.account = account;
                    }
                    self.reading = false;
                    match result {
                        Ok(page) => {
                            if let Err(error) = self.absorb(page) {
                                self.fail(error);
                            }
                        }
                        Err(error) => self.fail(error),
                    }
                    changed = true;
                }
                Envelope::Sent(generation, id, result) if generation == self.generation => {
                    self.writing = false;
                    if matches!(
                        result,
                        Delivery::AccountChanged | Delivery::AuthorizationLost
                    ) {
                        let reason = if matches!(result, Delivery::AuthorizationLost) {
                            "authorization_lost"
                        } else {
                            "account_changed"
                        };
                        self.reset();
                        self.error = Some(transport_problem(reason));
                        changed = true;
                        continue;
                    }
                    // The ledger may already contain this message before its HTTP receipt arrives.
                    if !self.outgoing.as_ref().is_some_and(|out| out.id == id) {
                        continue;
                    }
                    match result {
                        Delivery::Receipt(state) => {
                            if let Some(out) = &mut self.outgoing {
                                out.state = state;
                            }
                            self.error = None;
                            self.last_refresh = None;
                        }
                        Delivery::Uncertain(error) => {
                            if let Some(out) = &mut self.outgoing {
                                out.state = "uncertain".into();
                                out.retryable = true;
                            }
                            self.send_error = Some(error);
                        }
                        Delivery::Rejected(error) => {
                            if let Some(mut out) = self.outgoing.take() {
                                if self.draft.is_empty() {
                                    self.draft = out.text.clone();
                                }
                                out.state = "refused".into();
                                self.rejected = Some(out);
                            }
                            self.send_error = Some(error);
                        }
                        Delivery::AccountChanged | Delivery::AuthorizationLost => unreachable!(),
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        if !Arc::ptr_eq(&previous_events, &self.events)
            || previous_outgoing != self.outgoing
            || previous_rejected != self.rejected
        {
            self.revision += 1;
        }
        changed
    }

    fn fail(&mut self, error: String) {
        self.online = false;
        self.more = false;
        self.error = Some(error);
    }

    fn absorb(&mut self, page: Page) -> Result<(), String> {
        let last = page
            .events
            .iter()
            .map(|e| e.seq)
            .max()
            .unwrap_or(self.cursor);
        if page.more && last <= self.cursor {
            return Err("대화의 다음 순번을 확인하지 못했어요. 다시 연결하는 중이에요.".into());
        }
        let mut ordered = BTreeMap::new();
        for event in page.events {
            if event.kind == "message"
                && self.outgoing.as_ref().is_some_and(|out| out.id == event.id)
            {
                self.outgoing = None;
            }
            if event.seq > self.cursor {
                ordered.entry(event.seq).or_insert(event);
            }
        }
        if !ordered.is_empty() {
            let events = Arc::make_mut(&mut self.events);
            events.extend(ordered.into_values());
            let mut bytes: usize = events.iter().map(|e| e.text.len()).sum();
            let mut drop = events.len().saturating_sub(RETAIN_EVENTS);
            bytes = bytes.saturating_sub(events[..drop].iter().map(|e| e.text.len()).sum());
            while bytes > RETAIN_BYTES && drop + 1 < events.len() {
                bytes = bytes.saturating_sub(events[drop].text.len());
                drop += 1;
            }
            events.drain(..drop);
        }
        // head_seq is the remote end, not the end of this page; jumping there loses missed replies.
        self.cursor = self.cursor.max(last);
        self.loaded = true;
        self.online = true;
        self.more = page.more;
        self.error = None;
        if self.outgoing.is_none() && self.rejected.is_none() {
            self.send_error = None;
        }
        Ok(())
    }

    pub(crate) fn scroll_by(&mut self, delta: f32) -> bool {
        if !delta.is_finite() {
            return false;
        }
        let max = self
            .layout
            .lock()
            .map(|layout| layout.scroll_max)
            .unwrap_or(0.0);
        let next = (self.scroll + delta).clamp(0.0, max);
        let changed = next != self.scroll;
        self.scroll = next;
        changed
    }
}

fn transport_problem(code: &str) -> String {
    match code {
        "account_changed" => "로그인이 바뀌어 이전 대화를 비웠어요. 계정 연결을 확인해 주세요.",
        "login_required" => "로그인 후 나쵸와 대화할 수 있어요. 설정에서 계정을 연결해 주세요.",
        "authorization_lost" => {
            "대화 권한을 확인하지 못해 이전 대화를 비웠어요. 계정 연결을 확인해 주세요."
        }
        "response_too_large" => "대화 응답이 너무 커서 받지 않았어요.",
        _ => "나쵸에 연결하지 못했어요. 마지막 대화는 남겨 두었어요.",
    }
    .into()
}

fn authorization_lost(status: u16) -> bool {
    matches!(status, 401 | 403)
}

fn word(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn parse_page(status: u16, bytes: &[u8]) -> Result<Page, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "나쵸 대화 응답을 읽지 못했어요. 마지막 대화는 남겨 두었어요.".to_string())?;
    if status != 200 {
        return Err(problem(status, &value));
    }
    let rows = value
        .get("events")
        .and_then(Value::as_array)
        .ok_or("나쵸 대화 목록이 올바르지 않아요. 앱 업데이트를 확인해 주세요.")?;
    if rows.len() > PAGE_ROWS {
        return Err("나쵸 대화 목록이 너무 커서 받지 않았어요.".into());
    }
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        for (key, limit) in [("id", 256), ("message", 256), ("kind", 64), ("state", 64), ("surface", 64)] {
            if row.get(key).and_then(Value::as_str).is_some_and(|value| value.len() > limit) {
                return Err("나쵸 메시지 정보가 너무 커서 받지 않았어요.".into());
            }
        }
        let seq = row
            .get("seq")
            .and_then(Value::as_u64)
            .filter(|seq| *seq > 0)
            .ok_or("나쵸 대화 순번이 올바르지 않아요.")?;
        let content = word(row, "text");
        let note = word(row, "note");
        if content.len() > 128 * 1024 || note.len() > 128 * 1024 {
            return Err("나쵸 메시지가 너무 커서 받지 않았어요.".into());
        }
        events.push(Event {
            seq,
            kind: word(row, "kind"),
            id: word(row, "id"),
            message: word(row, "message"),
            text: if content.is_empty() { note } else { content },
            state: word(row, "state"),
            surface: word(row, "surface"),
        });
    }
    Ok(Page {
        events,
        more: value
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(rows.len() >= PAGE_ROWS),
    })
}

fn parse_receipt(status: u16, bytes: &[u8]) -> Delivery {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return Delivery::Uncertain(
            "접수 영수증을 읽지 못했어요. 같은 메시지로 다시 확인해 주세요.".into(),
        );
    };
    if status == 200 && value.get("ok").and_then(Value::as_bool) == Some(true) {
        let Some(state) = value
            .pointer("/receipt/state")
            .and_then(Value::as_str)
            .filter(|state| !state.is_empty())
        else {
            return Delivery::Uncertain(
                "접수 상태가 비어 있어요. 같은 메시지로 다시 확인해 주세요.".into(),
            );
        };
        return Delivery::Receipt(state.to_string());
    }
    if status >= 500 || status == 408 || status == 429 {
        Delivery::Uncertain(problem(status, &value))
    } else {
        Delivery::Rejected(problem(status, &value))
    }
}

fn problem(status: u16, value: &Value) -> String {
    match value.get("error").and_then(Value::as_str).unwrap_or("") {
        "stale_rev" => "작업이 바뀌었어요. 최신 작업을 다시 선택한 뒤 보내 주세요.",
        "task_closed" => "이미 닫힌 작업이에요. 작업 연결을 해제하면 일반 대화로 보낼 수 있어요.",
        "task_elsewhere" | "no_task" => {
            "연결된 작업을 확인하지 못했어요. 작업을 다시 선택해 주세요."
        }
        "secret_like" => "비밀 정보처럼 보이는 글은 대화 기록에 남길 수 없어요.",
        "too_long" => "메시지가 너무 길어요. 나누어서 보내 주세요.",
        "id_conflict" => "같은 메시지 번호에 다른 내용이 있어 접수하지 않았어요.",
        "bad_token" | "unauthorized" | "session_expired" => {
            "로그인을 다시 확인해 주세요. 대화를 읽거나 보내지 못했어요."
        }
        "owner_only" | "not_owner" => "이 계정에는 나쵸 대화 권한이 없어요.",
        "app_key_missing" | "nacho_key_missing" => {
            "이 기기의 나쵸 연결이 준비되지 않았어요. 계정 연결을 확인해 주세요."
        }
        "nacho_unconfigured" | "nacho_unreachable" => {
            "나쵸에 연결하지 못했어요. 마지막 대화는 남겨 두었어요."
        }
        "nacho_hub_unavailable" => "나쵸가 연결된 기기를 켜고 계정 연결을 확인해 주세요.",
        _ if status == 404 => "나쵸 대화 창구를 찾지 못했어요. 연결과 앱 버전을 확인해 주세요.",
        _ if status == 401 || status == 403 => "로그인 또는 대화 권한을 확인해 주세요.",
        _ => "나쵸 응답을 확인하지 못했어요. 연결을 다시 확인해 주세요.",
    }
    .to_string()
}

fn receipt_label(state: &str) -> &str {
    match state {
        "sending" => "보내는 중…",
        "accepted" => "접수됨",
        "queued" => "대기 중",
        "running" => "답하는 중…",
        "answered" => "답변 완료",
        "failed" => "실패",
        "refused" => "접수 거절",
        "interrupted" => "중단됨",
        "restart" => "나쵸 재시작 중",
        "uncertain" => "접수 확인 필요",
        _ => "상태 확인 중",
    }
}

fn source_label(event: &Event) -> String {
    let own = event.kind == "message";
    let who = if own { "나" } else { "나쵸" };
    let origin = match event.surface.as_str() {
        "discord" => " · Discord",
        "slack" => " · Slack",
        "pet" => " · 펫",
        _ => "",
    };
    format!("{who}{origin}")
}

fn wrap(value: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in value.split('\n') {
        let mut line = String::new();
        let mut line_chars = 0;
        let mut advance = 0.0;
        for ch in paragraph.chars() {
            if ch == '\r' {
                continue;
            }
            let mut encoded = [0; 4];
            let next_advance = measure(ch.encode_utf8(&mut encoded)).max(0.0);
            // Zero-width text must not grow an unbounded shaping run on the render thread.
            if line_chars >= 256 || (!line.is_empty() && advance + next_advance > width.max(1.0)) {
                lines.push(std::mem::take(&mut line));
                line_chars = 0;
                advance = 0.0;
            }
            line.push(ch);
            line_chars += 1;
            advance += next_advance;
        }
        lines.push(line);
    }
    lines
}

fn rows(
    snapshot: &ChatSnapshot,
    width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> (Vec<PaintRow>, f32) {
    let mut rows = Vec::new();
    let mut y = 0.0;
    for event in snapshot.events.iter().filter(|event| {
        matches!(
            event.kind.as_str(),
            "message" | "reply" | "progress" | "notice" | "status"
        )
    }) {
        if event.text.is_empty() {
            continue;
        }
        let own = event.kind == "message";
        let mut label = source_label(event);
        if own {
            if let Some(status) = snapshot
                .events
                .iter()
                .rev()
                .find(|row| row.kind == "status" && row.message == event.id)
            {
                label.push_str(" · ");
                label.push_str(receipt_label(&status.state));
            }
        }
        let lines = wrap(&event.text, width, &mut measure);
        let height = 18.0 + lines.len() as f32 * 18.0 + 16.0;
        rows.push(PaintRow {
            y,
            label,
            lines,
            own,
            muted: matches!(event.kind.as_str(), "progress" | "notice"),
        });
        y += height;
    }
    for out in snapshot.rejected.iter().chain(snapshot.outgoing.iter()) {
        let lines = wrap(&out.text, width, &mut measure);
        let height = 18.0 + lines.len() as f32 * 18.0 + 16.0;
        rows.push(PaintRow {
            y,
            label: format!("나 · {}", receipt_label(&out.state)),
            lines,
            own: true,
            muted: false,
        });
        y += height;
    }
    (rows, y)
}

pub(super) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    area: Rect,
) {
    let chat = &s.chat;
    let (x, y, w, h) = area;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    g.push_clip(x, y, w, h);
    let label = fit(g, "나쵸와 대화", w - 82.0, 14.0, true);
    text(g, x, y + 6.0, &label, 14.0, theme::text(), true);
    let connection = if chat.online {
        "연결됨"
    } else if chat.loading {
        "연결 중…"
    } else {
        "연결 확인"
    };
    let cw = g.measure_chrome_text(connection, 10.5, false);
    text(
        g,
        x + w - cw,
        y + 8.0,
        connection,
        10.5,
        if chat.online {
            theme::success()
        } else {
            theme::text_dim()
        },
        false,
    );
    divider(g, x, y + 32.0, w);
    let context_h = if let Some((task, _)) = &chat.context {
        let label = fit(g, &format!("작업: {task}"), w - 86.0, 10.5, false);
        text(g, x, y + 48.0, &label, 10.5, theme::text_dim(), false);
        action(
            g,
            s,
            hits,
            (x + w - 76.0, y + 40.0, 76.0, 26.0),
            "연결 해제",
            Target::NachoClearContext,
            false,
            true,
        );
        40.0
    } else {
        0.0
    };
    let hint = chat
        .error
        .as_deref()
        .unwrap_or("Enter로 전송 · 비밀 정보는 보내지 마세요");
    let hints = wrap(hint, w, |value| g.measure_chrome_text(value, 10.5, false));
    let hint_extra = hints.len().saturating_sub(1) as f32 * 18.0;
    let composer_y = (y + h - 126.0 - hint_extra).max(y + 40.0 + context_h);
    let transcript = (
        x,
        y + 40.0 + context_h,
        w,
        (composer_y - y - 40.0 - context_h - 6.0).max(0.0),
    );
    g.push_clip(transcript.0, transcript.1, transcript.2, transcript.3);
    if let Ok(mut cache) = chat.layout.lock() {
        let key = (chat.revision, w.to_bits(), theme::ui_font_gen());
        if cache.key != key {
            let (rows, height) = rows(chat, (w - 20.0).max(1.0), |value| {
                g.measure_chrome_text(value, 12.0, false)
            });
            cache.rows = rows;
            cache.height = height;
            cache.key = key;
        }
        cache.scroll_max = (cache.height - transcript.3).max(0.0);
        let offset = cache.scroll_max - chat.scroll.min(cache.scroll_max);
        for row in &cache.rows {
            let top = transcript.1 + row.y - offset;
            let bottom = top + 18.0 + row.lines.len() as f32 * 18.0;
            if bottom < transcript.1 || top > transcript.1 + transcript.3 {
                continue;
            }
            text(
                g,
                x + 10.0,
                top,
                &row.label,
                10.5,
                if row.own {
                    theme::accent()
                } else {
                    theme::text_dim()
                },
                false,
            );
            for (index, line) in row.lines.iter().enumerate() {
                let line_y = top + 18.0 + index as f32 * 18.0;
                if line_y + 18.0 >= transcript.1 && line_y < transcript.1 + transcript.3 {
                    text(
                        g,
                        x + 10.0,
                        line_y,
                        line,
                        12.0,
                        if row.muted {
                            theme::text_dim()
                        } else {
                            theme::text()
                        },
                        false,
                    );
                }
            }
        }
        if cache.rows.is_empty() {
            let message = if chat.loading && !chat.loaded {
                "대화를 불러오는 중…"
            } else if chat.loaded {
                "나쵸에게 말을 걸어 보세요.\n작업을 고르면 그 일에 이어서 이야기해요."
            } else {
                "연결되면 최근 대화가 여기에 표시돼요."
            };
            for (index, line) in wrap(message, w - 20.0, |value| {
                g.measure_chrome_text(value, 12.0, false)
            })
            .iter()
            .enumerate()
            {
                text(
                    g,
                    x + 10.0,
                    transcript.1 + 10.0 + index as f32 * 18.0,
                    line,
                    12.0,
                    theme::text_dim(),
                    false,
                );
            }
        }
        if cache.scroll_max > 0.0 && transcript.3 > 0.0 {
            let thumb_h = (transcript.3 * transcript.3 / cache.height)
                .max(26.0)
                .min(transcript.3);
            let thumb_y = transcript.1 + (transcript.3 - thumb_h) * offset / cache.scroll_max;
            g.rect(x + w - 3.0, thumb_y, 3.0, thumb_h, theme::border());
        }
    }
    g.pop_clip();
    divider(g, x, composer_y, w);
    text(
        g,
        x,
        composer_y + 8.0,
        "메시지",
        10.5,
        theme::text_dim(),
        false,
    );
    field(
        g,
        s,
        hits,
        caret,
        (x, composer_y + 26.0, w, 40.0),
        "작업을 정리해 줘…",
        &chat.draft,
        BoardInput::NachoMessage,
    );
    for (index, hint) in hints.iter().enumerate() {
        text(
            g,
            x,
            composer_y + 72.0 + index as f32 * 18.0,
            hint,
            10.5,
            if chat.error.is_some() {
                theme::danger()
            } else {
                theme::text_dim()
            },
            false,
        );
    }
    let controls_y = composer_y + 96.0 + hint_extra;
    let retry = chat.outgoing.as_ref().is_some_and(|out| out.retryable);
    if retry {
        action(
            g,
            s,
            hits,
            (x, controls_y, (w - 82.0).min(160.0), 26.0),
            "같은 메시지 재확인",
            Target::NachoRetry,
            false,
            true,
        );
    } else if !chat.authenticated {
        action(
            g,
            s,
            hits,
            (x, controls_y, 76.0, 26.0),
            "로그인",
            Target::NachoLogin,
            false,
            true,
        );
    }
    let sending = chat
        .outgoing
        .as_ref()
        .is_some_and(|out| out.state == "sending");
    action(
        g,
        s,
        hits,
        (x + w - 76.0, controls_y, 76.0, 26.0),
        if sending {
            "전송 중…"
        } else {
            "보내기"
        },
        Target::NachoSend,
        true,
        chat.online && chat.outgoing.is_none() && !chat.draft.trim().is_empty(),
    );
    g.pop_clip();
}

fn action(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    rect: Rect,
    label: &str,
    target: Target,
    primary: bool,
    enabled: bool,
) {
    let rect = crate::native_controls::text_button(
        g,
        rect,
        s.cursor,
        label,
        crate::native_controls::Style {
            primary,
            enabled,
            ..Default::default()
        },
    );
    if enabled {
        hit(g, hits, target, rect, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(value: Value) -> Page {
        parse_page(200, &serde_json::to_vec(&value).unwrap()).unwrap()
    }

    #[test]
    fn oversized_metadata_cannot_escape_transcript_retention_bounds() {
        for (field, limit) in [("id", 256), ("message", 256), ("kind", 64), ("state", 64), ("surface", 64)] {
            let mut event = json!({"seq":1,"kind":"reply","text":"ok"});
            event[field] = Value::String("x".repeat(limit + 1));
            assert!(parse_page(200, &serde_json::to_vec(&json!({"events":[event]})).unwrap()).is_err());
        }
    }

    #[test]
    fn cursor_uses_received_page_not_remote_head_and_deduplicates_reconnect() {
        let mut state = ChatState::default();
        state.absorb(page(json!({"events":[{"seq":3,"kind":"reply","text":"셋"},{"seq":2,"kind":"reply","text":"둘"}],"head_seq":900,"has_more":true}))).unwrap();
        assert_eq!(state.cursor, 3);
        state.absorb(page(json!({"events":[{"seq":3,"kind":"reply","text":"셋"},{"seq":4,"kind":"reply","text":"넷"}],"head_seq":900,"has_more":false}))).unwrap();
        assert_eq!(
            state.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![2, 3, 4]
        );
        assert_eq!(state.cursor, 4);
        assert!(!state.more);
    }

    #[test]
    fn retention_is_bounded_without_replaying_evicted_rows() {
        let mut state = ChatState::default();
        let events: Vec<Value> = (1..=500)
            .map(|seq| json!({"seq":seq,"kind":"reply","text":"답"}))
            .collect();
        state
            .absorb(page(json!({"events":events,"has_more":false})))
            .unwrap();
        assert_eq!(state.events.len(), RETAIN_EVENTS);
        assert_eq!(state.cursor, 500);
        state
            .absorb(page(
                json!({"events":[{"seq":1,"kind":"reply","text":"old"}],"has_more":false}),
            ))
            .unwrap();
        assert_eq!(state.events.first().unwrap().seq, 301);
        assert!(state
            .absorb(page(json!({"events":[],"has_more":true})))
            .is_err());
    }

    #[test]
    fn retry_payload_keeps_id_text_and_original_task_revision() {
        let mut outgoing = Outgoing {
            id: "stable-id".into(),
            text: "원래 내용".into(),
            task: Some(("task-1".into(), "8".into())),
            state: "uncertain".into(),
            retryable: true,
        };
        let before = outgoing.body();
        outgoing.state = "sending".into();
        outgoing.retryable = false;
        assert_eq!(outgoing.body(), before);
        assert_eq!(
            before,
            json!({"id":"stable-id","text":"원래 내용","task":"task-1","rev":"8"})
        );
    }

    #[test]
    fn a_lost_receipt_is_not_claimed_as_failed_or_safe_to_resend_as_new() {
        assert!(matches!(
            parse_receipt(200, b"bad json"),
            Delivery::Uncertain(_)
        ));
        assert!(matches!(
            parse_receipt(200, br#"{"ok":true}"#),
            Delivery::Uncertain(_)
        ));
        assert!(matches!(
            parse_receipt(503, br#"{"error":"unavailable"}"#),
            Delivery::Uncertain(_)
        ));
        assert!(matches!(
            parse_receipt(409, br#"{"error":"stale_rev"}"#),
            Delivery::Rejected(_)
        ));
        assert!(
            matches!(parse_receipt(200, br#"{"ok":true,"receipt":{"state":"queued"}}"#), Delivery::Receipt(state) if state == "queued")
        );
    }

    #[test]
    fn ledger_confirmation_wins_a_later_transport_failure() {
        let mut state = ChatState::default();
        state.outgoing = Some(Outgoing {
            id: "id-1".into(),
            text: "내용".into(),
            task: None,
            state: "sending".into(),
            retryable: false,
        });
        state.writing = true;
        state
            .absorb(page(
                json!({"events":[{"seq":1,"kind":"message","id":"id-1","text":"내용"}]}),
            ))
            .unwrap();
        state.mailbox.lock().unwrap().push(Envelope::Sent(
            0,
            "id-1".into(),
            Delivery::Uncertain("끊김".into()),
        ));
        state.pump();
        assert!(state.outgoing.is_none());
        assert!(!state.writing);
        assert!(state.error.is_none());
    }

    #[test]
    fn reset_rejects_an_old_workers_account_transcript() {
        let mut state = ChatState::default();
        let old = Arc::clone(&state.mailbox);
        state.draft = "이전 계정 초안".into();
        state.reset();
        old.lock().unwrap().push(Envelope::Read(
            0,
            None,
            Ok(page(
                json!({"events":[{"seq":1,"kind":"reply","text":"old-account"}]}),
            )),
        ));
        assert!(!state.pump());
        assert!(state.draft.is_empty() && state.events.is_empty());
    }

    #[test]
    fn unknown_remote_error_text_never_gets_reflected_into_ui() {
        let secret = "private-server-token";
        let error = problem(500, &json!({"error":secret}));
        assert!(!error.contains(secret));
        assert!(parse_page(200, br#"{"events":[{"kind":"reply"}]}"#).is_err());
    }

    #[test]
    fn korean_long_words_and_newlines_wrap_without_losing_text() {
        let text = "작업정리해주세요\n두 번째 줄";
        let lines = wrap(text, 4.0, |s| s.chars().count() as f32);
        assert!(lines.iter().all(|line| line.chars().count() <= 4));
        assert_eq!(lines.concat(), text.replace('\n', ""));
        assert_eq!(wrap("\n", 10.0, |s| s.len() as f32), vec!["", ""]);
    }

    #[test]
    fn an_account_change_discards_old_envelopes_already_drained_together() {
        let mut state = ChatState::default();
        state.outgoing = Some(Outgoing {
            id: "id-1".into(),
            text: "old".into(),
            task: None,
            state: "sending".into(),
            retryable: false,
        });
        state.mailbox.lock().unwrap().extend([
            Envelope::Sent(0, "id-1".into(), Delivery::AccountChanged),
            Envelope::Read(
                0,
                None,
                Ok(page(
                    json!({"events":[{"seq":1,"kind":"reply","text":"old-account"}]}),
                )),
            ),
        ]);
        state.pump();
        assert!(state.events.is_empty());
        assert!(state.outgoing.is_none());
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn refusal_preserves_a_new_draft_and_its_reason_across_successful_polls() {
        let mut state = ChatState::default();
        state.draft = "작성 중인 다음 말".into();
        state.outgoing = Some(Outgoing {
            id: "id-1".into(),
            text: "거절된 말".into(),
            task: None,
            state: "sending".into(),
            retryable: false,
        });
        state.mailbox.lock().unwrap().push(Envelope::Sent(
            0,
            "id-1".into(),
            Delivery::Rejected("작업이 바뀌었어요".into()),
        ));
        state.pump();
        state
            .absorb(page(json!({"events":[],"has_more":false})))
            .unwrap();
        assert_eq!(state.draft, "작성 중인 다음 말");
        assert_eq!(state.rejected.as_ref().unwrap().text, "거절된 말");
        assert_eq!(state.snapshot().error.as_deref(), Some("작업이 바뀌었어요"));
        assert!(state.outgoing.is_none());
    }

    #[test]
    fn signed_out_and_fixture_actions_cannot_start_transport_workers() {
        let mut state = ChatState::default();
        state.draft = "보내지 않음".into();
        state.send();
        assert_eq!(state.draft, "보내지 않음");
        assert!(state.outgoing.is_none());
        assert!(!state.writing);
        state.set_fixture();
        state.draft = "검증용 초안".into();
        state.refresh();
        state.send();
        state.retry();
        assert!(!state.reading && !state.writing && !state.refresh_due());
        assert_eq!(state.draft, "검증용 초안");
    }

    #[test]
    fn scroll_stays_inside_the_transcript_and_rejects_nan() {
        let mut state = ChatState::default();
        state.layout.lock().unwrap().scroll_max = 120.0;
        assert!(state.scroll_by(80.0));
        assert_eq!(state.scroll, 80.0);
        assert!(state.scroll_by(500.0));
        assert_eq!(state.scroll, 120.0);
        assert!(!state.scroll_by(f32::NAN));
        assert!(state.scroll_by(-500.0));
        assert_eq!(state.scroll, 0.0);
    }

    #[test]
    fn transcript_retention_is_also_bounded_by_bytes() {
        let mut state = ChatState::default();
        let content = "a".repeat(64 * 1024);
        let events: Vec<_> = (1..=20)
            .map(|seq| json!({"seq":seq,"kind":"reply","text":content}))
            .collect();
        state
            .absorb(page(json!({"events":events,"has_more":false})))
            .unwrap();
        assert_eq!(state.cursor, 20);
        assert!(
            state
                .events
                .iter()
                .map(|event| event.text.len())
                .sum::<usize>()
                <= RETAIN_BYTES
        );
    }

    #[test]
    fn permission_loss_clears_private_messages_draft_and_context() {
        assert!(authorization_lost(401) && authorization_lost(403));
        assert!(!authorization_lost(500));
        let mut state = ChatState::default();
        state.set_fixture();
        state.fixture = false;
        state.draft = "private draft".into();
        state.context = Some(("private-task".into(), "1".into()));
        state.outgoing = Some(Outgoing {
            id: "pending".into(),
            text: "private pending".into(),
            task: state.context.clone(),
            state: "sending".into(),
            retryable: false,
        });
        state
            .mailbox
            .lock()
            .unwrap()
            .push(Envelope::AuthorizationLost(state.generation));
        state.pump();
        assert!(state.events.is_empty() && state.draft.is_empty());
        assert!(state.context.is_none() && state.outgoing.is_none() && state.rejected.is_none());
        assert!(!state.online && !state.loaded && !state.snapshot().authenticated);
        assert!(state
            .error
            .as_deref()
            .is_some_and(|error| error.contains("권한")));
    }

    #[test]
    fn permission_loss_from_a_late_send_clears_even_an_already_confirmed_message() {
        let mut state = ChatState::default();
        state.set_fixture();
        state.fixture = false;
        assert!(state.outgoing.is_none());
        state.mailbox.lock().unwrap().push(Envelope::Sent(
            state.generation,
            "fixture-1".into(),
            Delivery::AuthorizationLost,
        ));
        state.pump();
        assert!(state.events.is_empty());
        assert!(!state.loaded);
    }

    #[test]
    fn offline_and_empty_polls_keep_history_and_do_not_relayout_transcript() {
        let mut state = ChatState::default();
        state.set_fixture();
        state.fixture = false;
        state.draft = "in progress".into();
        let revision = state.revision;
        let previous = Arc::clone(&state.events);
        state.mailbox.lock().unwrap().push(Envelope::Read(
            state.generation,
            None,
            Err(problem(500, &json!({"error":"offline"}))),
        ));
        state.pump();
        assert_eq!(state.draft, "in progress");
        assert!(Arc::ptr_eq(&previous, &state.events));
        assert_eq!(state.revision, revision);
        state.mailbox.lock().unwrap().push(Envelope::Read(
            state.generation,
            None,
            Ok(page(json!({"events":[],"has_more":false}))),
        ));
        state.pump();
        assert_eq!(state.revision, revision);
        assert!(Arc::ptr_eq(&previous, &state.events));
        assert!(state.online);
    }

    #[test]
    fn zero_width_combining_text_has_linear_measurement_work_and_bounded_runs() {
        let input = "\u{301}".repeat(65_536);
        let mut measured_chars = 0;
        let lines = wrap(&input, 300.0, |value| {
            measured_chars += value.chars().count();
            0.0
        });
        assert_eq!(measured_chars, 65_536);
        assert!(lines.iter().all(|line| line.chars().count() <= 256));
        assert_eq!(lines.concat(), input);
    }
}
