//! 나쵸 작업 장부의 타입 클라이언트 — 데스크톱 할 일 판이 읽는 쪽.
//!
//! 정본은 나쵸의 `nacho/services/worklog.py` 이고, 여기서 읽는 창구는 앱용
//! `GET /api/app/tasks` · `GET /api/app/tasks/{id}`(`nacho/services/appview.py`)다. 폰 중계
//! (`kasa_mcp` 의 nacho_relay)와 **같은 서술자·키**를 써서 두 화면이 같은 장부를 본다.
//!
//! 이 파일이 지키는 것:
//! - **끝남과 성공을 가른다.** `done` 은 장부가 닫혔다는 뜻일 뿐이다(직접 답한 일은 확인 없이
//!   닫힌다). 성공은 상세의 `verify.ok == true` 가 있을 때만이고, 기록이 없으면 「검증 안 됨」이다.
//! - **같은 일은 한 줄.** 같은 id 가 두 번 오면 `rev`(장부 `updated` 의 ms)가 큰 쪽만 남기고,
//!   늦게 도착한 옛 응답이 새 줄을 덮지 못하게 이전 판과도 비교한다.
//! - **끊겨도 지우지 않는다.** 장부에 못 닿으면 마지막으로 받은 목록을 그대로 두고 「오래됨」과
//!   이유만 붙인다. 다시 닿으면 전체 목록을 새로 받는다(변경 피드가 아직 없어 이어받을 커서가 없다).
//! - **가짜는 가짜라고 적는다.** 검증용 가상 장부는 `BookSource::Fixture` 로만 만들어지고 화면이
//!   그 사실을 표시한다.
//!
//! 승인은 읽기만 한다. 장부의 `approval` 은 `{needed, what}` 뿐이라 누가·어디까지·언제까지·몇 번을
//! 담지 못한다 — 그 계약이 서버에 생기기 전에는 이 창구로 승인을 보내지 않는다.

use std::collections::HashMap;
use std::io::{Read, Write};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TaskState {
    Queued,
    Working,
    Verifying,
    RestartPending,
    PostRestartVerifying,
    ApprovalNeeded,
    Done,
    Failed,
    Cancelled,
    /// 모르는 낱말 — 나쵸가 상태를 늘려도 목록이 통째로 안 깨지게 받아 두고 「미확인」으로 보인다.
    #[default]
    Unknown,
}

impl TaskState {
    pub(crate) fn parse(value: &str) -> Self {
        match value {
            "queued" => Self::Queued,
            "working" => Self::Working,
            "verifying" => Self::Verifying,
            "restart_pending" => Self::RestartPending,
            "post_restart_verifying" => Self::PostRestartVerifying,
            "approval_needed" => Self::ApprovalNeeded,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => Self::Unknown,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Queued => "대기",
            Self::Working => "진행 중",
            Self::Verifying => "확인 중",
            Self::RestartPending => "재시작 대기",
            Self::PostRestartVerifying => "재시작 뒤 확인",
            Self::ApprovalNeeded => "승인 대기",
            Self::Done => "끝남",
            Self::Failed => "실패",
            Self::Cancelled => "접음",
            Self::Unknown => "상태 미확인",
        }
    }

    pub(crate) const fn closed(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

impl<'de> serde::Deserialize<'de> for TaskState {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = Option::<String>::deserialize(de)?;
        Ok(value.as_deref().map(Self::parse).unwrap_or_default())
    }
}

/// 장부의 `watch` — 이 일을 맡은 창. `surface` 는 `%N` 이라 기계 id 와 함께만 뜻이 있고
/// 재사용된다. `surface_key`(판 주소의 UUID)가 오면 그것이 창의 정본이다.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskPlace {
    pub(crate) surface: Option<String>,
    pub(crate) surface_key: Option<String>,
    /// 그 창이 다른 일로 넘어갔으면 그 일 id — 이때는 이 일에 창을 잇지 않는다.
    pub(crate) superseded_by: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) machine_id: String,
}

/// 목록 한 줄(`appview.card`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskCard {
    pub(crate) id: String,
    pub(crate) goal: String,
    pub(crate) state: TaskState,
    pub(crate) state_label: String,
    pub(crate) group: String,
    pub(crate) project: String,
    /// 일이 들어온 창구 — 「카사모바일」「디스코드」「펫(…)」.
    pub(crate) place: String,
    pub(crate) step: String,
    /// 사람 손이 필요한 이유 한 줄(승인 대기·막힘·멈춰 둠). 비면 없음.
    pub(crate) attention: String,
    pub(crate) paused: bool,
    pub(crate) student: Option<TaskPlace>,
    pub(crate) created_ms: u64,
    pub(crate) updated_ms: u64,
    pub(crate) rev: String,
    /// 아래 넷은 데스크 범위 계약(2026-09-25 합의)에서 오는 칸. 옛 나쵸는 안 싣는다.
    /// `direct`·`self`·`other` — 직접 답한 일(`direct`)은 확인 없이 닫힌다.
    pub(crate) kind: Option<String>,
    /// 같은 판(rev)에 장부가 적은 마지막 검증. 없으면 `None` — 통과로 채우지 않는다.
    pub(crate) verify_ok: Option<bool>,
    pub(crate) run_id: Option<String>,
    pub(crate) approval: Option<TaskApproval>,
}

impl TaskCard {
    pub(crate) fn machine_id(&self) -> Option<&str> {
        self.student.as_ref().map(|s| s.machine_id.as_str()).filter(|m| !m.is_empty())
    }

    pub(crate) fn surface(&self) -> Option<&str> {
        self.student.as_ref().and_then(|s| s.surface.as_deref()).filter(|s| !s.is_empty())
    }

    pub(crate) fn surface_key(&self) -> Option<&str> {
        self.student.as_ref().and_then(|s| s.surface_key.as_deref()).filter(|s| !s.is_empty())
    }

    /// 창이 다른 일로 넘어갔다 — 장부가 그렇게 말하면 창 번호가 맞아도 잇지 않는다.
    pub(crate) fn superseded(&self) -> bool {
        self.student.as_ref().and_then(|s| s.superseded_by.as_deref()).is_some_and(|id| !id.is_empty())
    }

    pub(crate) fn needs_you(&self) -> bool {
        self.state == TaskState::ApprovalNeeded || (!self.state.closed() && !self.attention.trim().is_empty())
    }

    /// 순서 비교용 판 번호. `rev` 가 숫자가 아니면 `updated_ms` 로 대신한다.
    fn rev_number(&self) -> u64 {
        self.rev.parse().unwrap_or(self.updated_ms)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskVerify {
    pub(crate) ok: bool,
    pub(crate) note: String,
    pub(crate) at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskReport {
    pub(crate) status: String,
    pub(crate) summary: String,
    pub(crate) changed: Vec<String>,
    pub(crate) tests: String,
    pub(crate) next: String,
    pub(crate) character: String,
    pub(crate) harness: String,
    pub(crate) at_ms: u64,
}

/// 승인. 지금 나쵸는 `{needed, what, note}` 만, 확정 모양은 1회용·범위·만료를 더 싣는다
/// (정본: 나쵸 레포 `docs/development/api/desk-api.md` 「승인」). 이 파일은 **읽기만** 한다 —
/// 결정 POST 는 기기 인증 결정이 나기 전까지 PC·폰 모두 부르지 않기로 했다.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskApproval {
    pub(crate) needed: bool,
    pub(crate) what: String,
    pub(crate) note: String,
    pub(crate) id: String,
    pub(crate) task_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) action: String,
    pub(crate) risk: String,
    pub(crate) scope_hash: String,
    pub(crate) created_at_ms: u64,
    pub(crate) expires_at_ms: u64,
    pub(crate) one_use: bool,
    pub(crate) approver: String,
    /// `pending`·`approved`·`denied`·`expired`.
    pub(crate) state: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskHistory {
    pub(crate) at_ms: u64,
    pub(crate) label: String,
    pub(crate) why: String,
}

/// 상세(`appview.detail`) 중 할 일 판이 쓰는 칸.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskDetail {
    pub(crate) id: String,
    pub(crate) rev: String,
    pub(crate) request: String,
    pub(crate) kind: Option<String>,
    pub(crate) remaining: Vec<String>,
    pub(crate) result: String,
    pub(crate) blocked: String,
    pub(crate) verify: Option<TaskVerify>,
    pub(crate) report: Option<TaskReport>,
    pub(crate) approval: Option<TaskApproval>,
    pub(crate) history: Vec<TaskHistory>,
}

/// 끝난 일에 대해 장부가 말하는 검증 결과. `None` 은 「기록 없음」이지 통과가 아니다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Verdict {
    Passed,
    Failed,
    #[default]
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BookSource {
    /// 아직 한 번도 안 물었다.
    #[default]
    Unasked,
    Live,
    Fixture,
}

/// 목록 한 번의 답.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub(crate) struct TaskList {
    pub(crate) tasks: Vec<TaskCard>,
    /// `app`(폰·펫 창구가 닿은 일만) 또는 `desk`(주인이 맡긴 일 전부). 옛 나쵸는 안 싣는다 — 그때는 `app` 이다.
    pub(crate) scope: String,
    /// 데스크 범위에서 사내 채널·남의 DM 에서 맡긴 일이라 싣지 않은 개수.
    pub(crate) hidden_count: u64,
    /// 목록 판 이름표 — 롱폴의 `since` 로 돌려준다(아직 이 판은 롱폴을 안 쓴다).
    pub(crate) board_rev: String,
    /// 롱폴이 바뀐 것 없이 끝났다 — `tasks` 가 안 실린다. 빈 목록으로 읽으면 판이 통째로 빈다.
    pub(crate) unchanged: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TaskBook {
    source: BookSource,
    scope: String,
    hidden_count: u64,
    tasks: Vec<TaskCard>,
    details: HashMap<String, TaskDetail>,
    checked_at_ms: u64,
    last_ok_ms: u64,
    error: Option<String>,
}

impl TaskBook {
    pub(crate) fn tasks(&self) -> &[TaskCard] {
        &self.tasks
    }

    pub(crate) fn detail(&self, id: &str) -> Option<&TaskDetail> {
        self.details.get(id)
    }

    pub(crate) fn source(&self) -> BookSource {
        self.source
    }

    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// 나쵸가 데스크 범위로 답했나. 아니면 디스코드·슬랙에서 맡긴 일이 목록에 없다.
    pub(crate) fn desk_scope(&self) -> bool {
        self.scope == "desk"
    }

    pub(crate) fn hidden_count(&self) -> u64 {
        self.hidden_count
    }

    pub(crate) fn checked_at_ms(&self) -> u64 {
        self.checked_at_ms
    }

    pub(crate) fn last_ok_ms(&self) -> u64 {
        self.last_ok_ms
    }

    /// 지금 보이는 목록이 마지막 물음의 답이 아니다(끊겼거나 읽지 못했다).
    pub(crate) fn stale(&self) -> bool {
        self.error.is_some() && !self.tasks.is_empty()
    }

    /// 카드의 `verify_ok` 가 먼저, 없으면 이 판(rev)의 상세 — 옛 판의 `verify` 로 새 판을
    /// 성공이라 부르지 않게.
    pub(crate) fn verdict(&self, card: &TaskCard) -> Verdict {
        let ok = card.verify_ok.or_else(|| {
            self.details.get(&card.id).filter(|d| d.rev == card.rev).and_then(|d| d.verify.as_ref()).map(|v| v.ok)
        });
        match ok {
            Some(true) => Verdict::Passed,
            Some(false) => Verdict::Failed,
            None => Verdict::None,
        }
    }

    /// 카드에 실린 승인이 먼저, 없으면 이 판의 상세.
    pub(crate) fn approval<'a>(&'a self, card: &'a TaskCard) -> Option<&'a TaskApproval> {
        card.approval.as_ref()
            .or_else(|| self.details.get(&card.id).filter(|d| d.rev == card.rev).and_then(|d| d.approval.as_ref()))
            .filter(|a| a.needed || a.state == "pending")
    }
}

/// 새 목록을 이전 판에 합친다. 같은 id 는 `rev` 가 큰 쪽 하나만, 이전 판보다 옛 줄은 이전 줄로.
/// 새 목록에 없는 id 는 버린다 — 목록이 정본이고, 창 밖으로 밀려난 일은 장부 쪽 판단이다.
pub(crate) fn merge_cards(previous: &[TaskCard], fresh: Vec<TaskCard>) -> Vec<TaskCard> {
    let mut by_id: HashMap<String, TaskCard> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for card in fresh.into_iter().filter(|c| !c.id.is_empty()) {
        match by_id.get(&card.id) {
            Some(kept) if kept.rev_number() >= card.rev_number() => {}
            Some(_) => {
                by_id.insert(card.id.clone(), card);
            }
            None => {
                order.push(card.id.clone());
                by_id.insert(card.id.clone(), card);
            }
        }
    }
    for old in previous {
        if let Some(new) = by_id.get_mut(&old.id) {
            if old.rev_number() > new.rev_number() {
                *new = old.clone();
            }
        }
    }
    order.into_iter().filter_map(|id| by_id.remove(&id)).collect()
}

/// 상세를 새로 받아야 하는 줄 — 판정이 상세에 달린 줄(끝남·승인·확인 중)이면서 캐시가 옛 판인 것.
fn wants_detail(card: &TaskCard, cached: Option<&TaskDetail>) -> bool {
    // 카드가 검증을 이미 싣고 있으면 끝난 줄은 상세가 판정을 안 바꾼다.
    if card.state.closed() && card.verify_ok.is_some() {
        return false;
    }
    let decisive = card.state.closed()
        || matches!(card.state, TaskState::ApprovalNeeded | TaskState::Verifying | TaskState::PostRestartVerifying)
        || card.needs_you();
    decisive && cached.is_none_or(|d| d.rev != card.rev)
}

/// 한 번 새로 고칠 때 상세를 몇 개까지 받나. 목록이 길어도 판 갱신 한 번이 나쵸를 두드리지 않게.
const DETAIL_BUDGET: usize = 6;

pub(crate) fn refresh(previous: &TaskBook) -> TaskBook {
    let now = kasa_socket::board::now_ms();
    refresh_with(previous, now, &HttpLedger)
}

/// 장부를 읽는 통로. 시험에서는 가짜로 갈아 끼운다.
pub(crate) trait Ledger {
    fn list(&self) -> Result<TaskList, String>;
    fn detail(&self, id: &str) -> Result<TaskDetail, String>;
}

pub(crate) fn refresh_with(previous: &TaskBook, now: u64, ledger: &dyn Ledger) -> TaskBook {
    let mut book = previous.clone();
    book.checked_at_ms = now;
    let reply = match ledger.list() {
        Ok(reply) => reply,
        Err(error) => {
            book.error = Some(error);
            if book.source == BookSource::Unasked {
                book.source = BookSource::Live;
            }
            return book;
        }
    };
    book.source = BookSource::Live;
    book.error = None;
    book.last_ok_ms = now;
    if reply.unchanged {
        return book;
    }
    book.scope = if reply.scope.is_empty() { "app".into() } else { reply.scope };
    book.hidden_count = reply.hidden_count;
    book.tasks = merge_cards(&previous.tasks, reply.tasks);
    book.details.retain(|id, _| book.tasks.iter().any(|c| &c.id == id));
    let wanted: Vec<String> = book.tasks.iter()
        .filter(|c| wants_detail(c, book.details.get(&c.id)))
        .take(DETAIL_BUDGET)
        .map(|c| c.id.clone())
        .collect();
    for id in wanted {
        match ledger.detail(&id) {
            Ok(detail) if detail.id == id => {
                book.details.insert(id, detail);
            }
            // 상세 하나가 안 돼도 목록은 산다 — 그 줄은 「검증 안 됨」으로 남는다.
            Ok(_) | Err(_) => {}
        }
    }
    book
}

struct HttpLedger;

impl Ledger for HttpLedger {
    fn list(&self) -> Result<TaskList, String> {
        // 데스크 범위를 먼저 청한다. 옛 나쵸는 모르는 질의를 무시하고 폰 범위로 답하며,
        // 그 사실은 답의 `scope` 가 비는 것으로 드러난다.
        let body = app_get("/api/app/tasks?scope=desk")?;
        serde_json::from_slice::<TaskList>(&body).map_err(|_| "나쵸 장부 응답을 읽지 못했어요".into())
    }

    fn detail(&self, id: &str) -> Result<TaskDetail, String> {
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("작업 id 형식이 달라요".into());
        }
        #[derive(serde::Deserialize)]
        struct One {
            task: TaskDetail,
        }
        let body = app_get(&format!("/api/app/tasks/{id}"))?;
        serde_json::from_slice::<One>(&body).map(|o| o.task).map_err(|_| "작업 상세를 읽지 못했어요".into())
    }
}

/// 나쵸 앱 창구 GET 한 번. 창구는 루프백·메시의 평문 HTTP 라 의존성 없이 소켓으로 부른다 —
/// 판 갱신 스레드가 비동기 런타임을 들고 있지 않다.
fn app_get(path: &str) -> Result<Vec<u8>, String> {
    let (url, key) = kasa_mcp::nacho_app_target().map_err(|code| match code {
        "nacho_key_missing" => "이 기기에는 나쵸 앱 키가 없어 장부를 읽지 않았어요".to_string(),
        _ => "나쵸 자리 정보가 없어 장부를 읽지 않았어요".to_string(),
    })?;
    let authority = url.strip_prefix("http://").ok_or("나쵸 주소가 평문 HTTP 가 아니라 읽지 않았어요")?;
    let authority = authority.split('/').next().unwrap_or("");
    let addr = std::net::ToSocketAddrs::to_socket_addrs(authority)
        .ok()
        .and_then(|mut a| a.next())
        .ok_or("나쵸 주소를 해석하지 못했어요")?;
    let timeout = std::time::Duration::from_millis(1500);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|_| "나쵸 장부에 연결하지 못했어요".to_string())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    // HTTP/1.0 으로 물어 청크 전송을 피한다 — 본문 끝은 연결이 닫히는 자리다.
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {authority}\r\nX-Nacho-Token: {key}\r\nX-Kasa-Owner: 1\r\nX-Kasa-User: desktop\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(|_| "나쵸 장부에 요청을 보내지 못했어요".to_string())?;
    let mut raw = Vec::new();
    stream.take(2 * 1024 * 1024).read_to_end(&mut raw).map_err(|_| "나쵸 장부 응답이 끊겼어요".to_string())?;
    split_response(&raw)
}

fn split_response(raw: &[u8]) -> Result<Vec<u8>, String> {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("나쵸 장부 응답이 잘렸어요")?;
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
    match status {
        200 => Ok(raw[head_end + 4..].to_vec()),
        401 | 403 => Err("나쵸 장부가 이 기기의 요청을 거절했어요".into()),
        503 => Err("나쵸 장부가 앱 창구를 열지 않았어요".into()),
        _ => Err(format!("나쵸 장부가 답하지 못했어요 ({status})")),
    }
}

/// 검증용 가상 장부. 격리된 디버그 리그에서만 불리고, 화면은 출처로 가짜임을 표시한다.
#[cfg(any(test, debug_assertions))]
pub(crate) fn fixture_book(at: u64) -> TaskBook {
    let card = |id: &str, goal: &str, state: &str, project: &str, place: &str, attention: &str, surface: Option<(&str, &str)>, age: u64| TaskCard {
        id: id.into(),
        goal: goal.into(),
        state: TaskState::parse(state),
        state_label: TaskState::parse(state).label().into(),
        project: project.into(),
        place: place.into(),
        attention: attention.into(),
        student: surface.map(|(machine, surface)| TaskPlace { surface: Some(surface.into()), machine_id: machine.into(), ..Default::default() }),
        updated_ms: at.saturating_sub(age),
        rev: at.saturating_sub(age).to_string(),
        ..Default::default()
    };
    let tasks = vec![
        card("wfix0001", "배포 전 마지막 확인에 승인이 필요해요", "approval_needed", "제품", "디스코드", "승인 대기: 원격 반영", Some(("device-a", "%2")), 240_000),
        card("wfix0002", "주문 목록 화면 정리", "working", "제품", "카사모바일", "", Some(("device-a", "%1")), 8_000),
        card("wfix0003", "안내 문서 갱신", "verifying", "문서", "펫(작업 컴퓨터)", "", None, 60_000),
        card("wfix0004", "검색 결과 정렬 수정", "done", "제품", "카사모바일", "", None, 900_000),
        card("wfix0005", "빠른 질문 답변", "done", "자료", "디스코드", "", None, 1_800_000),
        card("wfix0006", "빌드 경고 정리", "failed", "제품", "디스코드", "", None, 3_600_000),
    ];
    let detail = |card: &TaskCard, verify: Option<bool>| TaskDetail {
        id: card.id.clone(),
        rev: card.rev.clone(),
        request: card.goal.clone(),
        verify: verify.map(|ok| TaskVerify { ok, note: if ok { "검사 12건 통과".into() } else { "검사 2건 실패".into() }, at_ms: card.updated_ms }),
        report: Some(TaskReport { summary: "변경을 남기고 결과를 보고했어요".into(), tests: "단위 검사 통과".into(), ..Default::default() }),
        approval: (card.state == TaskState::ApprovalNeeded).then(|| TaskApproval { needed: true, what: "원격 반영".into(), one_use: true, state: "pending".into(), ..Default::default() }),
        ..Default::default()
    };
    let mut details = HashMap::new();
    for (card, verify) in tasks.iter().zip([None, None, None, Some(true), None, Some(false)]) {
        details.insert(card.id.clone(), detail(card, verify));
    }
    TaskBook { source: BookSource::Fixture, scope: "desk".into(), hidden_count: 2, tasks, details, checked_at_ms: at, last_ok_ms: at, error: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn card(id: &str, state: &str, rev: u64) -> TaskCard {
        TaskCard { id: id.into(), state: TaskState::parse(state), rev: rev.to_string(), updated_ms: rev, ..Default::default() }
    }

    struct Fake {
        list: RefCell<Result<TaskList, String>>,
        details: HashMap<String, TaskDetail>,
        asked: RefCell<Vec<String>>,
    }

    impl Ledger for Fake {
        fn list(&self) -> Result<TaskList, String> {
            self.list.borrow().clone()
        }
        fn detail(&self, id: &str) -> Result<TaskDetail, String> {
            self.asked.borrow_mut().push(id.into());
            self.details.get(id).cloned().ok_or_else(|| "없음".into())
        }
    }

    fn fake(list: Result<Vec<TaskCard>, String>, details: Vec<TaskDetail>) -> Fake {
        Fake {
            list: RefCell::new(list.map(|tasks| TaskList { tasks, ..Default::default() })),
            details: details.into_iter().map(|d| (d.id.clone(), d)).collect(),
            asked: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn unknown_states_parse_without_breaking_the_list() {
        let cards: Vec<TaskCard> = serde_json::from_str(r#"[{"id":"w1","state":"teleporting"},{"id":"w2","state":null},{"id":"w3","state":"post_restart_verifying"}]"#).unwrap();
        assert_eq!(cards[0].state, TaskState::Unknown);
        assert_eq!(cards[1].state, TaskState::Unknown);
        assert_eq!(cards[2].state, TaskState::PostRestartVerifying);
    }

    #[test]
    fn duplicates_keep_the_newest_revision_and_late_replies_cannot_regress() {
        let merged = merge_cards(&[], vec![card("w1", "working", 5), card("w1", "verifying", 9), card("w2", "working", 1)]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].state, TaskState::Verifying, "같은 id 는 큰 rev 하나");
        let late = merge_cards(&merged, vec![card("w1", "working", 5)]);
        assert_eq!(late[0].state, TaskState::Verifying, "늦게 온 옛 응답이 새 판을 덮지 않는다");
        assert_eq!(late.len(), 1, "새 목록에 없는 id 는 목록이 정본이라 빠진다");
    }

    #[test]
    fn done_is_not_success_without_a_passing_verify_of_the_same_revision() {
        let done = card("w1", "done", 10);
        let mut book = TaskBook { tasks: vec![done.clone()], ..Default::default() };
        assert_eq!(book.verdict(&done), Verdict::None, "기록이 없으면 통과가 아니다");
        book.details.insert("w1".into(), TaskDetail { id: "w1".into(), rev: "9".into(), verify: Some(TaskVerify { ok: true, ..Default::default() }), ..Default::default() });
        assert_eq!(book.verdict(&done), Verdict::None, "옛 판의 통과를 새 판에 쓰지 않는다");
        book.details.insert("w1".into(), TaskDetail { id: "w1".into(), rev: "10".into(), verify: Some(TaskVerify { ok: true, ..Default::default() }), ..Default::default() });
        assert_eq!(book.verdict(&done), Verdict::Passed);
    }

    #[test]
    fn offline_keeps_the_last_list_and_marks_it_stale_then_recovers() {
        let first = refresh_with(&TaskBook::default(), 100, &fake(Ok(vec![card("w1", "working", 1)]), vec![]));
        assert_eq!(first.source(), BookSource::Live);
        assert!(!first.stale());
        let offline = refresh_with(&first, 200, &fake(Err("끊김".into()), vec![]));
        assert_eq!(offline.tasks().len(), 1, "끊겨도 지우지 않는다");
        assert!(offline.stale());
        assert_eq!(offline.last_ok_ms(), 100, "마지막으로 닿은 시각은 그대로");
        let back = refresh_with(&offline, 300, &fake(Ok(vec![card("w1", "verifying", 2)]), vec![]));
        assert!(!back.stale());
        assert_eq!(back.tasks()[0].state, TaskState::Verifying);
    }

    #[test]
    fn never_reached_is_empty_not_stale() {
        let book = refresh_with(&TaskBook::default(), 100, &fake(Err("키 없음".into()), vec![]));
        assert!(book.tasks().is_empty());
        assert!(!book.stale(), "보여 준 적 없는 목록은 오래될 수도 없다");
        assert_eq!(book.error(), Some("키 없음"));
    }

    #[test]
    fn details_are_fetched_once_per_revision_for_decisive_rows() {
        let list = vec![card("w1", "done", 3), card("w2", "working", 3)];
        let detail = TaskDetail { id: "w1".into(), rev: "3".into(), ..Default::default() };
        let ledger = fake(Ok(list.clone()), vec![detail]);
        let book = refresh_with(&TaskBook::default(), 1, &ledger);
        assert_eq!(*ledger.asked.borrow(), vec!["w1".to_string()], "진행 중인 줄은 상세가 판정을 안 바꾼다");
        let again = fake(Ok(list), vec![]);
        refresh_with(&book, 2, &again);
        assert!(again.asked.borrow().is_empty(), "같은 판이면 다시 묻지 않는다");
    }

    #[test]
    fn desk_contract_fields_are_read_and_old_servers_fall_back_to_app_scope() {
        let reply: TaskList = serde_json::from_str(r#"{"ok":true,"scope":"desk","hidden_count":3,"board_rev":"b9",
            "tasks":[{"id":"w1","state":"done","rev":"5","kind":"direct","verify_ok":null,"run_id":null,
              "student":{"surface":"%3","surface_key":"k-1","superseded_by":"w2","machine_id":"m"},
              "approval":{"id":"a1","action":"push","scope_hash":"h","expires_at_ms":9,"one_use":true,"approver":"owner","state":"pending"}}]}"#).unwrap();
        assert_eq!((reply.scope.as_str(), reply.hidden_count, reply.board_rev.as_str()), ("desk", 3, "b9"));
        let card = reply.tasks[0].clone();
        assert_eq!(card.verify_ok, None, "null 은 통과가 아니다");
        assert_eq!(card.surface_key(), Some("k-1"));
        assert!(card.superseded());
        assert_eq!(card.approval.as_ref().map(|a| (a.one_use, a.state.as_str())), Some((true, "pending")));
        let desk = refresh_with(&TaskBook::default(), 1, &Fake { list: RefCell::new(Ok(reply)), details: HashMap::new(), asked: RefCell::new(Vec::new()) });
        assert!(desk.desk_scope());
        let old = refresh_with(&TaskBook::default(), 1, &fake(Ok(vec![card.clone()]), vec![]));
        assert!(!old.desk_scope(), "scope 를 안 싣는 옛 나쵸는 폰 범위다");
    }

    #[test]
    fn unchanged_long_poll_keeps_the_list() {
        let first = refresh_with(&TaskBook::default(), 1, &fake(Ok(vec![card("w1", "working", 1)]), vec![]));
        let same = TaskList { unchanged: true, scope: "desk".into(), ..Default::default() };
        let kept = refresh_with(&first, 2, &Fake { list: RefCell::new(Ok(same)), details: HashMap::new(), asked: RefCell::new(Vec::new()) });
        assert_eq!(kept.tasks().len(), 1, "목록 없는 답은 「그대로」다");
        assert_eq!(kept.last_ok_ms(), 2);
    }

    /// 나쵸가 내준 응답 fixture(구현된 모양·목표 모양) 전부를 이 파일의 타입으로 읽는다.
    /// fixture 는 나쵸 레포에 있어 `NACHO_DESK_FIXTURES=<그 폴더>` 로 가리켜 `--ignored` 로 돈다.
    #[test]
    #[ignore]
    fn nacho_desk_fixtures_parse() {
        let dir = std::path::PathBuf::from(std::env::var("NACHO_DESK_FIXTURES").expect("NACHO_DESK_FIXTURES"));
        let read = |name: &str| std::fs::read(dir.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        for name in ["tasks.app.implemented.json", "tasks.desk.planned.json"] {
            let list: TaskList = serde_json::from_slice(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!list.tasks.is_empty(), "{name}");
            assert!(list.tasks.iter().all(|c| !c.id.is_empty() && c.state != TaskState::Unknown), "{name}");
        }
        let desk: TaskList = serde_json::from_slice(&read("tasks.desk.planned.json")).unwrap();
        assert_eq!(desk.scope, "desk");
        assert!(desk.tasks.iter().any(|c| c.surface_key().is_some()), "목표 모양은 창 열쇠를 싣는다");
        let unchanged: TaskList = serde_json::from_slice(&read("tasks.desk.unchanged.planned.json")).unwrap();
        assert!(unchanged.unchanged && unchanged.tasks.is_empty());
        for name in ["task.detail.implemented.json", "task.detail.desk.planned.json"] {
            #[derive(serde::Deserialize)]
            struct One { task: TaskDetail }
            let one: One = serde_json::from_slice(&read(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!one.task.id.is_empty(), "{name}");
        }
        #[derive(serde::Deserialize)]
        struct Planned { task: TaskCard }
        let planned: Planned = serde_json::from_slice(&read("task.detail.desk.planned.json")).unwrap();
        if let Some(a) = planned.task.approval.as_ref() {
            assert!(!a.id.is_empty() && a.expires_at_ms > 0, "확정 승인 칸: {a:?}");
        }
    }

    #[test]
    fn card_verify_wins_and_skips_the_detail_request() {
        let mut done = card("w1", "done", 4);
        done.verify_ok = Some(false);
        let ledger = fake(Ok(vec![done.clone()]), vec![TaskDetail { id: "w1".into(), rev: "4".into(), verify: Some(TaskVerify { ok: true, ..Default::default() }), ..Default::default() }]);
        let book = refresh_with(&TaskBook::default(), 1, &ledger);
        assert!(ledger.asked.borrow().is_empty(), "카드가 검증을 실으면 상세를 안 묻는다");
        assert_eq!(book.verdict(&book.tasks()[0]), Verdict::Failed);
    }

    #[test]
    fn response_status_is_checked_before_the_body_is_trusted() {
        assert_eq!(split_response(b"HTTP/1.0 200 OK\r\nA: b\r\n\r\n{}").unwrap(), b"{}");
        assert!(split_response(b"HTTP/1.0 403 Forbidden\r\n\r\n{\"tasks\":[]}").is_err());
        assert!(split_response(b"HTTP/1.0 200 OK").is_err(), "머리가 안 끝난 응답은 버린다");
    }

    /// 실기 확인 — 나쵸와 앱 키가 있는 기계에서 `--ignored` 로 돌린다. 창구의 헤더·응답 모양이
    /// 이 파일과 어긋나면 여기서 먼저 걸린다.
    #[test]
    #[ignore]
    fn live_ledger_answers_with_cards() {
        let book = refresh(&TaskBook::default());
        assert_eq!(book.error(), None);
        for card in book.tasks() {
            assert!(!card.id.is_empty() && card.state != TaskState::Unknown, "{card:?}");
        }
        eprintln!("live tasks: {:?}", book.tasks().iter().map(|c| (&c.id, c.state, book.verdict(c))).collect::<Vec<_>>());
    }

    #[test]
    fn fixture_is_labeled_and_separates_ended_from_verified() {
        let book = fixture_book(10_000_000);
        assert_eq!(book.source(), BookSource::Fixture);
        let verdicts: Vec<_> = book.tasks().iter().filter(|c| c.state.closed()).map(|c| book.verdict(c)).collect();
        assert!(verdicts.contains(&Verdict::Passed) && verdicts.contains(&Verdict::None), "끝남과 성공이 둘 다 보여야 한다");
    }
}
