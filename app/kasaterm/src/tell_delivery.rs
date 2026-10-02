use super::*;
use kasa_socket::tell::{Address, Hold, Record, State};
use kasa_socket::Backend;
use std::time::Duration;
use std::collections::{BTreeMap,HashSet};
use std::sync::atomic::{AtomicBool,AtomicUsize,Ordering};

/// 붙여넣기가 화면에 닿았는지 보는 앞머리 길이(공백 제외). 짧은 본문은 통째로 확인된다.
const PROBE_CHARS: usize = 24;

const PROOF_DEADLINE: Duration = Duration::from_secs(2);
/// 붙여넣은 글이 화면에 그려지기를 기다리는 한도. 막 뜬 claude 는 첫 붙여넣기를 160ms 안에
/// 못 그려, 한 번만 보고 Enter 를 보류하던 자리다(2026-09-28 실측: 입력 변화 없이 echoed=false).
const ECHO_DEADLINE: Duration = Duration::from_secs(2);
const ECHO_POLL: Duration = Duration::from_millis(160);
const PROOF_FRESHNESS: Duration = Duration::from_millis(250);
/// 붙여넣기부터 Enter 까지 사람 입력을 붙들어 두는 한도 — 반영 기다림과 매 회차 신원 증명을 덮는다.
/// 보통은 그보다 훨씬 먼저 `release_input` 이 푼다.
const HOLD_FOR: Duration = Duration::from_secs(4);
const WORKERS: usize = 4;
/// 키를 친 뒤 이만큼은 화면이 아직 못 따라왔을 수 있어 초안 표시를 믿는다. 에코는 수 ms, 이미지 붙여넣기의
/// 첨부 표시는 1초 안팎이라 넉넉히 잡았다. 그 뒤는 화면이 정본이다 — Esc·Ctrl+C 로 비운 입력칸이 Enter 를
/// 칠 때까지 초안으로 남아 tell 을 15분 붙들다 버리던 자리다(2026-10-01, 하루 61건 중 21건).
const DRAFT_TRUST: Duration = Duration::from_secs(5);

#[derive(Clone,Debug)]
struct Proof {
    completed: Instant,
    binding_epoch: u64,
    harness: kasa_pty::AgentKind,
}

#[derive(Clone)]
pub(crate) struct Commit {
    record: Record,
    pty: Arc<kasa_pty::PtySession>,
    revision: u64,
    proof: std::result::Result<Proof,String>,
    /// 붙여넣은 시각 — 반영을 기다릴 한도의 기준. 붙여넣기 전에는 `None`.
    pasted_at: Option<Instant>,
}

#[derive(Default)]
struct Scheduler { cursor: Option<String>, active: HashSet<String> }

impl Scheduler {
    fn select(&mut self, pending: Vec<Record>, limit: usize) -> Vec<Record> {
        let mut recipients = BTreeMap::<String,Record>::new();
        for record in pending {
            if !self.active.contains(&record.address.surface_id) {
                recipients.entry(record.address.surface_id.clone()).or_insert(record);
            }
        }
        let mut ordered: Vec<_> = recipients.into_iter().collect();
        if let Some(cursor) = &self.cursor {
            let next = ordered.partition_point(|(surface,_)|surface <= cursor);
            let len = ordered.len();
            if len > 0 { ordered.rotate_left(next % len); }
        }
        ordered.into_iter().take(limit).map(|(surface,record)| {
            self.cursor = Some(surface.clone()); self.active.insert(surface); record
        }).collect()
    }
}

fn scheduler() -> &'static std::sync::Mutex<Scheduler> {
    static VALUE: std::sync::OnceLock<std::sync::Mutex<Scheduler>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

fn release(surface: &str) { scheduler().lock().unwrap().active.remove(surface); }

fn finish(record: &Record, state: State, reason: &str) {
    let record = record.clone(); let reason = reason.to_owned();
    std::thread::spawn(move || {
        let _ = kasa_mcp::tell_service::transition(&record.message_id,state,&reason);
        release(&record.address.surface_id);
    });
}

fn deadline_proof<T: Send + 'static>(deadline: Duration, collect: impl FnOnce() -> std::result::Result<T,String> + Send + 'static) -> std::result::Result<T,String> {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    if ACTIVE.fetch_update(Ordering::AcqRel,Ordering::Acquire,|n|(n < WORKERS).then_some(n+1)).is_err() {
        return Err("identity proof workers are occupied".into());
    }
    let (tx,rx) = std::sync::mpsc::channel();
    struct Permit(&'static AtomicUsize);
    impl Drop for Permit { fn drop(&mut self) { self.0.fetch_sub(1,Ordering::AcqRel); } }
    let permit = Permit(&ACTIVE);
    std::thread::spawn(move || {
        let _permit = permit;
        let result = collect();
        let _ = tx.send(result);
    });
    rx.recv_timeout(deadline).map_err(|_|"identity proof deadline expired".to_string())?
}

fn collect_proof(backend: Arc<crate::socket::PtyBackend>, record: Record, pty: Arc<kasa_pty::PtySession>) -> std::result::Result<Proof,String> {
    deadline_proof(PROOF_DEADLINE,move || {
        let epoch = backend.tell_binding_epoch(&record.address.surface_id);
        let identity = backend.collab_tell_identity(&record.address.surface_id).map_err(|e|e.to_string())?;
        if Address::parse(&identity).ok().as_ref() != Some(&record.address)
            || identity["agent_pid"].as_u64() != Some(record.receiver_agent_pid as u64)
            || backend.tell_binding_epoch(&record.address.surface_id) != epoch
            || !kasa_pty::lookup_session(&record.address.surface_id).is_some_and(|current|Arc::ptr_eq(&current,&pty)) {
            return Err("live target session, binding or PTY changed".into());
        }
        let harness = identity["harness"].as_str().and_then(kasa_pty::AgentKind::from_id)
            .filter(|kind|matches!(kind,kasa_pty::AgentKind::Claude | kasa_pty::AgentKind::Codex))
            .ok_or_else(||"unsupported harness identity".to_owned())?;
        Ok(Proof { completed:Instant::now(),binding_epoch:epoch,harness })
    })
}

/// 지금 화면 한 장을 행 배열로.
fn live_cells(pty: &kasa_pty::PtySession) -> (Vec<Vec<GridCell>>, kasa_bridge::screen::ScreenUpdate) {
    let screen = pty.live_screen();
    let mut cells = vec![Vec::new();screen.rows as usize];
    for (index,row) in &screen.dirty {
        if let Some(target) = cells.get_mut(*index as usize) { *target = row.clone(); }
    }
    (cells,screen)
}

/// 입력창 안의 글 — 붙여넣은 글이 거기 들어갔는지 보는 자리. 입력창을 못 찾으면 `None`.
fn input_box_text(cells: &[Vec<GridCell>]) -> Option<String> {
    let rows = crate::screenread::prompt_box(cells)?.rows();
    Some(cells[rows].iter().map(|row|row.iter().map(|cell|cell.ch).filter(|ch|*ch != '\0').chain(['\n']).collect::<String>()).collect())
}

#[derive(Debug,PartialEq,Eq)]
enum CommitStep { Enter, Wait, Withhold }

/// 입력이 그대로인데 글만 아직 안 그려졌으면 조금 더 본다. 누가 끼어들면 `unchanged` 가 먼저
/// 깨지므로, 기다림이 남의 입력 위에 Enter 를 치는 일은 없다.
fn commit_step(unchanged: bool, echoed: bool, since_paste: Option<Duration>) -> CommitStep {
    match (unchanged,echoed) {
        (true,true) => CommitStep::Enter,
        (true,false) if since_paste.is_some_and(|waited|waited < ECHO_DEADLINE) => CommitStep::Wait,
        _ => CommitStep::Withhold,
    }
}

/// 붙여넣은 뒤 잠시 두고 신원을 다시 증명해 GUI 에 Enter 판정을 맡긴다 — 반영을 기다리는 동안 되풀이된다.
fn schedule_commit(backend: Arc<crate::socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>, mut commit: Commit) {
    std::thread::spawn(move || {
        std::thread::sleep(ECHO_POLL);
        commit.proof = collect_proof(backend,commit.record.clone(),commit.pty.clone());
        if proxy.send_event(UserEvent::SafeTellCommit(commit.clone())).is_err() {
            let _ = commit.pty.release_input();
            finish(&commit.record,State::Uncertain,"GUI stopped after paste; automatic retry prohibited");
        }
    });
}

fn begin_proof(backend: Arc<crate::socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>, record: Record) {
    std::thread::spawn(move || {
        let Some(pty) = kasa_pty::lookup_session(&record.address.surface_id) else {
            finish(&record,State::Failed,"live target PTY disappeared"); return;
        };
        let revision = pty.input_revision();
        let proof = collect_proof(backend,record.clone(),pty.clone());
        if let Err(reason) = &proof {
            if reason.contains("occupied") || reason.contains("deadline") { release(&record.address.surface_id); }
            else { finish(&record,State::Failed,reason); }
            return;
        }
        if kasa_mcp::tell_service::transition(&record.message_id,State::Dispatching,"identity proven; awaiting guarded first write").is_err() {
            release(&record.address.surface_id); return;
        }
        let delivery = Commit {record,pty,revision,proof,pasted_at:None};
        if proxy.send_event(UserEvent::SafeTellReady(delivery.clone())).is_err() {
            finish(&delivery.record,State::Failed,"GUI stopped before the first write");
        }
    });
}
/// 줄 선 채 이만큼 지나면 보낸 창에 한 번 알린다. 전달은 보통 1초 안에 끝나고(10-01 하루치 중앙값 0.8초),
/// 그보다 길면 받는 쪽 사람이 풀어야 하는 것이다. 짧은 초안 정리로 울리지 않을 만큼만 둔다.
const NOTICE_AFTER: Duration = Duration::from_secs(120);
const WATCH_POLL: Duration = Duration::from_secs(20);

/// 보낸 쪽지 하나 — 보낸 창에 알릴 때까지 지켜본다.
struct Watch {
    id: String,
    address: serde_json::Value,
    notify: String,
    label: String,
    first_line: String,
    sent_at: Instant,
    warned: bool,
    next_poll: Instant,
}

fn watches() -> &'static std::sync::Mutex<Vec<Watch>> {
    static VALUE: std::sync::OnceLock<std::sync::Mutex<Vec<Watch>>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

/// 이 기기 창이 보낸 쪽지를 맡는다. 보낸 쪽은 `tell --status` 를 따로 보지 않으면 막힌 줄도 버려진 줄도
/// 몰랐다(2026-10-01: 15분 만료 두 건을 받는 쪽이 먼저 알아챘다).
pub(crate) fn watch_sent(receipt: &serde_json::Value, notify: &serde_json::Value, params: &serde_json::Value) {
    if !matches!(receipt["state"].as_str(), Some("accepted" | "dispatching")) { return; }
    let (Some(id), Some(surface)) = (receipt["message_id"].as_str(), notify["surface"].as_str()) else { return };
    let label = notify["label"].as_str().or_else(||receipt["address"]["surface_id"].as_str()).unwrap_or("받는 창");
    let body = params["body"].as_str().unwrap_or_default();
    let line = body.lines().map(str::trim).find(|line|!line.is_empty()).unwrap_or_default();
    let line = line.strip_prefix('⟦').and_then(|rest|rest.split_once('⟧')).map_or(line,|(_,rest)|rest.trim_start());
    let mut first_line: String = line.chars().take(40).collect();
    if line.chars().count() > 40 { first_line.push('…'); }
    let now = Instant::now();
    watches().lock().unwrap().push(Watch {
        id: id.into(), address: receipt["address"].clone(), notify: surface.into(), label: label.into(),
        first_line, sent_at: now, warned: false, next_poll: now + WATCH_POLL,
    });
}

/// 지켜보는 쪽지마다 영수증을 다시 묻고, 알릴 것이 생기면 보낸 창에 tell 한다. 다른 기기 영수증은 HTTP 라
/// 늦을 수 있어 전달 틱과 다른 스레드에서 돈다.
fn poll_watches(backend: Arc<crate::socket::PtyBackend>, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    let now = Instant::now();
    let due: Vec<(String,serde_json::Value)> = watches().lock().unwrap().iter()
        .filter(|watch|watch.next_poll <= now).map(|watch|(watch.id.clone(),watch.address.clone())).collect();
    if due.is_empty() || ACTIVE.swap(true,Ordering::AcqRel) { return; }
    std::thread::spawn(move || {
        for (id,address) in due {
            let receipt = kasa_mcp::tell_service::status(&serde_json::json!({"message_id":id,"address":address}));
            let notice = {
                let mut list = watches().lock().unwrap();
                let Some(index) = list.iter().position(|watch|watch.id == id) else { continue };
                let watch = &mut list[index];
                watch.next_poll = Instant::now() + WATCH_POLL;
                let step = match &receipt {
                    Ok(receipt) => watch_step(watch,receipt),
                    // 영수증을 못 물으면(다른 기기가 꺼짐 등) 다음 차례에 다시 본다 — 하루가 지나면 영수증째 없다.
                    Err(_) if watch.sent_at.elapsed() > Duration::from_millis(kasa_socket::tell::RECEIPT_LIFETIME_MS) => WatchStep::Drop(None),
                    Err(_) => WatchStep::Keep(None),
                };
                match step {
                    WatchStep::Keep(notice) => notice.map(|text|(watch.notify.clone(),text)),
                    WatchStep::Drop(notice) => {
                        let notify = watch.notify.clone();
                        list.remove(index);
                        notice.map(|text|(notify,text))
                    }
                }
            };
            if let Some((surface,text)) = notice {
                let params = serde_json::json!({"message_id":kasa_socket::tell::new_message_id(),"surface_id":surface,"body":text});
                let wake = ||proxy.send_event(UserEvent::SafeTellWake).map_err(|_|anyhow::anyhow!("GUI delivery event loop stopped"));
                if let Err(error) = kasa_mcp::tell_service::submit(&*backend,&params,wake) {
                    eprintln!("[tell] 보낸 창 {surface} 에 쪽지 알림 실패: {error:#}");
                }
            }
        }
        ACTIVE.store(false,Ordering::Release);
    });
}

enum WatchStep { Keep(Option<String>), Drop(Option<String>) }

fn watch_step(watch: &mut Watch, receipt: &serde_json::Value) -> WatchStep {
    let reason = receipt["reason"].as_str().unwrap_or_default();
    let head = format!("{} 에게 보낸 쪽지({})", watch.label, watch.id);
    let first = &watch.first_line;
    match receipt["state"].as_str() {
        Some("submitted") => WatchStep::Drop(None),
        Some("failed") => {
            let why = if reason == "queued message expired" { "기다리다 만료됐어요".to_string() } else { reason.to_string() };
            WatchStep::Drop(Some(format!("[쪽지 못 감] {head}가 못 들어가고 버려졌어요 — {why}. 첫 줄: «{first}». 필요하면 새로 보내세요.")))
        }
        Some("uncertain") => WatchStep::Drop(Some(format!(
            "[쪽지 확인 못 함] {head}가 들어갔는지 확인 못 했어요 — {reason}. 새로 보내지 말고 `kasaterm-cli tell --status {}` 로 같은 ID 만 확인하세요. 첫 줄: «{first}»",
            watch.id))),
        Some("accepted") if !watch.warned && watch.sent_at.elapsed() >= NOTICE_AFTER => {
            watch.warned = true;
            let until = receipt["expires_at_ms"].as_u64().and_then(kasa_socket::tell::clock_hm).unwrap_or_else(||"만료 시각".into());
            let (cause,remedy) = Hold::from_reason(reason).map_or(("받는 창이 아직 못 받았어요(옛 판이라 까닭을 안 알려 줘요)","받는 창이 비면 들어가요"),
                |hold|(hold.cause(),hold.remedy()));
            WatchStep::Keep(Some(format!(
                "[쪽지 대기] {head}가 {}분째 못 들어갔어요 — {cause}. {remedy}. {until}까지 못 들어가면 버려져요. 급하면 다른 길로 알리세요. 첫 줄: «{first}»",
                watch.sent_at.elapsed().as_secs() / 60)))
        }
        _ => WatchStep::Keep(None),
    }
}

/// tell 로 들어간 「지금 일」 — 받는 pane 의 다음 턴 시작 훅이 가져가 claude 세션 이름으로 쓴다. 입력칸에
/// `/rename` 을 쳐 넣으면 사람이 쓰던 글과 섞일 수 있어, 프롬프트 제출 훅의 공식 `sessionTitle` 로 간다.
/// claude 가 바쁘면 그 글은 줄을 섰다 제출되므로 한 시간까지 기다린다.
fn session_titles() -> &'static std::sync::Mutex<HashMap<String,(String,Instant)>> {
    static VALUE: std::sync::OnceLock<std::sync::Mutex<HashMap<String,(String,Instant)>>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

pub(crate) fn take_session_title(surface: &str) -> Option<String> {
    let (title,at) = session_titles().lock().unwrap().remove(surface)?;
    (at.elapsed() < Duration::from_secs(kasa_socket::tell::QUEUE_TTL_SECONDS)).then_some(title)
}

/// 칸 안 mod 에게 맡긴 tell 의 「지금 일」 — mod 가 넣었다고 답하면 그 창 이름을 바꾼다.
fn module_titles() -> &'static std::sync::Mutex<HashMap<String,String>> {
    static VALUE: std::sync::OnceLock<std::sync::Mutex<HashMap<String,String>>> = std::sync::OnceLock::new();
    VALUE.get_or_init(Default::default)
}

/// mod 의 결과(ack)를 GUI 로 잇는다. 앱이 뜰 때 한 번 건다.
pub(crate) fn listen_module_acks(proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    kasa_mcp::claude_mod::set_ack_listener(move |surface, id, submitted| {
        let title = module_titles().lock().unwrap().remove(id);
        match title {
            Some(title) if submitted => { let _ = proxy.send_event(UserEvent::SocketRename(surface.to_string(),title)); }
            Some(_) => { session_titles().lock().unwrap().remove(surface); }
            None => {}
        }
        let _ = proxy.send_event(UserEvent::SafeTellWake);
    });
}

/// 받는 pane 입력박스 아래에 뜨는 한 줄 — 긴 것, 좁은 칸용 짧은 것.
pub(crate) fn waiting_label(count: usize, hold: Hold) -> [String; 2] {
    let short = match hold {
        Hold::Draft => "입력칸 비우기",
        Hold::Composition => "조합 끝내기",
        Hold::Approval => "승인·질문 답하기",
        Hold::Typing | Hold::Identity => "곧 들어감",
        Hold::Closed | Hold::PasteMode => "지금은 못 넣음",
        Hold::Busy => "일 끝나면",
    };
    [format!("쪽지 {count} 대기 · {}", hold.remedy()), format!("쪽지 대기 · {short}")]
}

/// 한 번 이상 미뤄진 쪽지를 받는 pane 별로 센다. 바뀐 때만 GUI 로 보낸다 — 틱은 0.5초마다 돈다.
fn publish_waiting(proxy: &winit::event_loop::EventLoopProxy<UserEvent>, pending: &[Record]) {
    static LAST: std::sync::Mutex<Option<HashMap<String,(usize,Hold)>>> = std::sync::Mutex::new(None);
    let waiting = waiting_by_surface(pending);
    let mut last = LAST.lock().unwrap();
    if last.as_ref() != Some(&waiting) && proxy.send_event(UserEvent::TellWaiting(waiting.clone())).is_ok() {
        *last = Some(waiting);
    }
}

fn waiting_by_surface(pending: &[Record]) -> HashMap<String,(usize,Hold)> {
    let mut waiting = HashMap::<String,(usize,Hold)>::new();
    for record in pending {
        let Some(hold) = Hold::from_reason(&record.reason) else { continue };
        let entry = waiting.entry(record.address.surface_id.clone()).or_insert((0,hold));
        entry.0 += 1;
    }
    waiting
}

impl std::fmt::Debug for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TellCommit").field("message_id",&self.record.message_id).finish()
    }
}

#[cfg(test)]
fn prompt_empty(cells: &[Vec<GridCell>], cursor_row: usize, harness: kasa_pty::AgentKind) -> bool {
    prompt_empty_at(cells,cursor_row,2,harness)
}

fn prompt_empty_at(cells: &[Vec<GridCell>], cursor_row: usize, cursor_col: usize, harness: kasa_pty::AgentKind) -> bool {
    use crate::screenread::PromptBox;
    use kasa_bridge::screen::Color;
    let Some(row) = cells.get(cursor_row) else { return false };
    let prompt = match harness { kasa_pty::AgentKind::Claude => '❯', kasa_pty::AgentKind::Codex => '›', _ => return false };
    let blank = |cell: &GridCell|cell.ch == '\0' || cell.ch.is_whitespace();
    let Some(marker) = row.iter().position(|cell|!blank(cell)) else { return false };
    if row[marker].ch != prompt { return false; }
    let region = crate::screenread::prompt_box(cells).and_then(|area| {
        let supported = matches!((&area,harness),
            (PromptBox::Bordered {..},kasa_pty::AgentKind::Claude)
            | (PromptBox::Filled {..},kasa_pty::AgentKind::Codex));
        let rows = area.rows();
        (supported && rows.contains(&cursor_row)).then_some(rows)
    });
    let mut placeholder = false;
    if let Some(first) = row.iter().enumerate().skip(marker+1).find(|(_,cell)|!blank(cell)).map(|(index,_)|index) {
        // A hint is not editable text: it starts at the insertion cursor,
        // has a distinct muted style and belongs to a recognized input box.
        let hint = &row[first];
        let muted = hint.dim || matches!(hint.fg,Color::Idx(8) | Color::Idx(240..=247))
            || matches!(hint.fg,Color::Rgb(r,g,b) if r == g && g == b && (80..=190).contains(&r));
        let distinct = hint.dim != row[marker].dim || hint.fg != row[marker].fg;
        placeholder = region.is_some() && cursor_col == first && muted && distinct
            && row[first..].iter().filter(|cell|!blank(cell)).all(|cell|
                cell.fg == hint.fg && cell.dim == hint.dim && !cell.bold && !cell.inverse && !cell.hidden);
        if !placeholder { return false; }
    }
    let input = region.clone().unwrap_or(cursor_row..cells.len());
    // Footer text is outside the bordered/filled input region. The identical
    // text inside that region remains a draft, regardless of its wording.
    for (index,row) in cells[input.clone()].iter().enumerate() {
        let absolute = input.start+index;
        if absolute == cursor_row { continue; }
        if row.iter().any(|cell|!blank(cell)) { return false; }
    }
    if !placeholder && row.iter().skip(marker+1).any(|cell|!blank(cell)) { return false; }
    let start = region.as_ref().map_or(cursor_row,|range|range.start);
    for row in cells[start.saturating_sub(3)..start].iter().rev() {
        let text: String = row.iter().map(|cell|cell.ch).collect();
        let lower = text.to_lowercase();
        if lower.contains("[image") || lower.contains("[attachment") || lower.contains("[pasted text")
            || lower.contains("image #") || text.contains('\u{fffc}') { return false; }
    }
    true
}

impl App {
    pub(crate) fn tell_composing(&self, surface: &str) -> bool {
        let owner = self.os_ime_surface.as_deref().or_else(||self.ime_focus.as_ref().and_then(crate::ImeFocus::terminal_surface));
        (self.in_preedit || !self.preedit.is_empty() || self.os_ime_surface.is_some())
            && owner.is_none_or(|owner|owner == surface || self.ws.lock().unwrap().active_tab_pid(owner) == surface)
    }

    /// 증명 뒤 대상이 바뀌었나 — 바뀌었으면 무엇이 바뀌었는지. 영수증에 그 말이 실린다.
    fn tell_target_change(&self, delivery: &Commit) -> Option<&'static str> {
        let Ok(proof) = &delivery.proof else { return Some("proof missing") };
        let surface = delivery.record.address.surface_id.as_str();
        if !self.socket_backend.as_ref().is_some_and(|backend|backend.tell_binding_epoch(surface) == proof.binding_epoch) {
            return Some("target transcript binding changed");
        }
        if kasa_mcp::surface_keys::get(surface).as_deref() != Some(delivery.record.address.surface_key.as_str()) {
            return Some("target surface identity changed");
        }
        // 탭 안의 학생도 받는다 — 바깥 pane 의 **활성 탭**이 아니라 그 surface 의 PTY 를 본다.
        // 활성 탭이 다른 학생이면 늘 「PTY 바뀜」으로 실패했다(2026-09-16 미니 미도리 실측).
        if !self.pty.get(surface).is_some_and(|current|Arc::ptr_eq(current,&delivery.pty)) {
            return Some("target PTY replaced");
        }
        if delivery.pty.input_closed() { return Some("target input closed"); }
        None
    }

    fn tell_proof_current(&self, delivery: &Commit) -> bool {
        delivery.proof.as_ref().is_ok_and(|proof|proof.completed.elapsed() <= PROOF_FRESHNESS)
            && delivery.pty.input_revision() == delivery.revision
            && delivery.record.expires_at_ms > kasa_socket::tell::now_ms()
    }

    fn tell_ready(&self, record: &Record, pty: &kasa_pty::PtySession, harness: kasa_pty::AgentKind, empty: bool) -> std::result::Result<(),Hold> {
        if pty.input_closed() { return Err(Hold::Closed); }
        // 붙여넣은 뒤(`empty` 가 거짓)에 시작된 한글 조합은 입력이 붙들려 있어 본문에 안 섞인다 — 그것 때문에
        // Enter 를 보류하면 본문이 입력창에 남아 사람 글과 함께 제출된다.
        if empty && self.tell_composing(&record.address.surface_id) { return Err(Hold::Composition); }
        let (cells,screen) = live_cells(pty);
        if !screen.bracketed_paste { return Err(Hold::PasteMode); }
        // 사람 차례(승인·질문)면 글을 안 넣는다 — 판정이 정본이고, 아래 화면 검사는 지금
        // 커서 아래에 승인 위젯이 그려져 있나 보는 기계적 보호막이다(배경 탭 포함).
        if self.collab.hub.state(&record.address.surface_id).needs_you()
            || crate::input::rows_show_approval_prompt(&cells).is_some() { return Err(Hold::Approval); }
        if !empty { return Ok(()); }
        if pty.input_draft_recent(DRAFT_TRUST) || !pty.input_quiet_for(Duration::from_millis(300)) { return Err(Hold::Typing); }
        if !prompt_empty_at(&cells,screen.cursor_row as usize,screen.cursor_col as usize,harness) { return Err(Hold::Draft); }
        Ok(())
    }

    pub(crate) fn safe_tell_tick(&mut self) {
        static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        static BATCH_ACTIVE: AtomicBool = AtomicBool::new(false);
        {
            let Ok(mut last) = LAST.lock() else { return };
            if last.is_some_and(|at|at.elapsed() < Duration::from_millis(500)) { return; }
            *last = Some(Instant::now());
        }
        let Some(backend) = self.socket_backend.clone() else { return };
        kasa_mcp::claude_mod::sweep_inbox();
        poll_watches(backend.clone(),self.proxy.clone());
        if BATCH_ACTIVE.swap(true,Ordering::AcqRel) { return; }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Ok(pending) = kasa_mcp::tell_service::pending() {
                publish_waiting(&proxy,&pending);
                let selected = scheduler().lock().unwrap().select(pending,WORKERS);
                for record in selected { begin_proof(backend.clone(),proxy.clone(),record); }
            }
            BATCH_ACTIVE.store(false,Ordering::Release);
        });
    }

    /// mod 가 실린 claude 칸이면 붙여넣지 않고 그 mod 에게 맡긴다 — mod 가 쉬는 순간 `$.prompt.submit` 으로 턴
    /// 하나를 시작하니 입력칸의 초안·한글 조합·승인 창과 겹칠 일이 없다. 맡았으면 참.
    fn tell_to_module(&self, delivery: &Commit) -> bool {
        let record = &delivery.record;
        let surface = record.address.surface_id.as_str();
        let Some(module) = kasa_mcp::claude_mod::live(surface).filter(|m| m.session == record.address.session_id) else { return false };
        if !self.tell_proof_current(delivery) {
            finish(record,if record.reject_if_busy { State::Failed } else { State::Accepted },Hold::Identity.reason());
            return true;
        }
        if !module.resting() || self.collab.hub.state(surface).needs_you() || kasa_mcp::claude_mod::has_offer(surface) {
            finish(record,if record.reject_if_busy { State::Failed } else { State::Accepted },Hold::Busy.reason());
            return true;
        }
        if !record.title.is_empty() {
            session_titles().lock().unwrap().insert(surface.to_string(),(record.title.clone(),Instant::now()));
            module_titles().lock().unwrap().insert(record.message_id.clone(),record.title.clone());
        }
        kasa_mcp::claude_mod::offer(surface,&module.session,&record.message_id,&record.body);
        // 영수증은 dispatching 으로 남고 mod 의 ack(또는 `sweep_inbox`)가 끝맺는다. 같은 칸의 다음 tell 은 그때까지
        // 대장(같은 칸에 dispatching 이 있으면 꺼내지 않음)이 붙든다.
        release(surface);
        true
    }

    pub(crate) fn safe_tell_ready(&mut self, delivery: &Commit) {
        if let Some(why) = self.tell_target_change(delivery) {
            eprintln!("[tell] {} → {} 실패: {why}", delivery.record.message_id, delivery.record.address.surface_id);
            finish(&delivery.record,State::Failed,&format!("{why} before the first write"));
            return;
        }
        let proof = delivery.proof.as_ref().unwrap();
        if proof.harness == kasa_pty::AgentKind::Claude && self.tell_to_module(delivery) { return; }
        let hold = if self.tell_proof_current(delivery) { self.tell_ready(&delivery.record,&delivery.pty,proof.harness,true).err() }
            else { Some(Hold::Identity) };
        if let Some(hold) = hold {
            let state = if delivery.record.reject_if_busy { State::Failed } else { State::Accepted };
            finish(&delivery.record,state,hold.reason());
            return;
        }
        let payload = format!("\x1b[200~{}\x1b[201~",delivery.record.body);
        // 붙여넣기와 Enter 사이에 사람이 친 글이 본문 뒤에 붙어 함께 제출되던 자리다(2026-09-29 「빈 입력창에 뭐 치면
        // 같이 전송」). 그 사이 입력은 붙들었다가 Enter 뒤(또는 보류 뒤) 친 순서대로 흘려보낸다.
        delivery.pty.hold_input(HOLD_FOR);
        let revision = match delivery.pty.send_bytes_guarded(payload.as_bytes(),Some(delivery.revision)) {
            Ok(revision) => revision,
            Err(_) => {
                let _ = delivery.pty.release_input();
                finish(&delivery.record,State::Uncertain,"paste write failed or input changed; automatic retry prohibited"); return;
            }
        };
        let Some(backend) = self.socket_backend.clone() else {
            let _ = delivery.pty.release_input();
            finish(&delivery.record,State::Uncertain,"receiver disappeared after paste"); return;
        };
        let mut commit = delivery.clone(); commit.revision = revision; commit.pasted_at = Some(Instant::now());
        schedule_commit(backend,self.proxy.clone(),commit);
    }

    pub(crate) fn safe_tell_commit(&mut self, commit: &Commit) {
        // 보류 사유를 하나로 뭉치면 영수증만 보고는 무엇이 막았는지 가를 수 없다(2026-09-29 두 건).
        let blocked = self.tell_target_change(commit)
            .or_else(||(commit.pty.input_revision() != commit.revision).then_some("input changed after paste"))
            .or_else(||(!self.tell_proof_current(commit)).then_some("identity proof stale or tell expired"))
            .or_else(||commit.proof.as_ref().ok().and_then(|proof|self.tell_ready(&commit.record,&commit.pty,proof.harness,false).err())
                .map(Hold::reason));
        let unchanged = blocked.is_none();
        // 입력창 안을 본다. 화면 맨 아래 30줄만 보면, 대화가 아직 없는 새 세션은 입력창이 화면
        // **위쪽**에 있어 큰 창에서 그 범위 밖이었다 — 글은 들어갔는데 에코를 못 찾아 Enter 를
        // 영영 보류했다(2026-09-28 실측: 44행 창, 입력창 9행). 입력창을 못 찾는 하네스만 옛 방식.
        let tail = input_box_text(&live_cells(&commit.pty).0).unwrap_or_else(||commit.pty.visible_text(30));
        let compact = |text: &str|text.chars().filter(|c|!c.is_whitespace()).collect::<String>();
        // 붙여넣은 글이 화면에 **통째로** 보여야 한다고 요구하면, 입력창이 접히거나 긴 본문이
        // tail 밖으로 밀린 자리에서 Enter 가 영영 안 나간다 — 글은 들어갔는데 제출만 안 된
        // 채로 끝난다(2026-09-21 「tell 엔터 안 되는 버그」). 앞머리만 본다: 내가 쓴 글이
        // 거기 있다는 증거로는 그것으로 충분하고, 이 길이가 남의 글과 우연히 겹치지 않는다.
        let probe: String = compact(&commit.record.body).chars().take(PROBE_CHARS).collect();
        let echoed = !probe.is_empty()
            && (compact(&tail).contains(&probe) || tail.contains("[Pasted text #"));
        match commit_step(unchanged,echoed,commit.pasted_at.map(|at|at.elapsed())) {
            CommitStep::Enter => {}
            CommitStep::Wait if self.socket_backend.is_some() => {
                schedule_commit(self.socket_backend.clone().unwrap(),self.proxy.clone(),commit.clone());
                return;
            }
            _ => {
                let why = blocked.unwrap_or("paste echo not seen");
                eprintln!("[tell] {} → {} Enter 보류: {why}", commit.record.message_id, commit.record.address.surface_id);
                let _ = commit.pty.release_input();
                finish(&commit.record,State::Uncertain,&format!("Enter withheld: {why}"));
                return;
            }
        }
        // 제목은 Enter **앞에** 맡긴다 — Enter 가 닿자마자 claude 의 제출 훅이 가져가러 온다.
        let surface = commit.record.address.surface_id.clone();
        if !commit.record.title.is_empty() {
            session_titles().lock().unwrap().insert(surface.clone(),(commit.record.title.clone(),Instant::now()));
        }
        let result = commit.pty.send_bytes_guarded(b"\r",Some(commit.revision));
        if result.is_err() { session_titles().lock().unwrap().remove(&surface); }
        let _ = commit.pty.release_input();
        let (state,reason) = if result.is_ok() { (State::Submitted,"paste and Enter writes succeeded; model read is unconfirmed") }
            else { (State::Uncertain,"Enter write unconfirmed; automatic retry prohibited") };
        finish(&commit.record,state,reason);
        // 새 일이 닿은 순간 그 창의 「지금 일」이 바뀐다 — 사이드바·창 머리·관측이 모두 창 이름을 읽는다.
        // claude 자기 세션 이름(`/resume`·agents 목록)은 그 글이 프롬프트로 제출될 때 훅이 맞춘다.
        if state == State::Submitted && !commit.record.title.is_empty() {
            let _ = self.proxy.send_event(UserEvent::SocketRename(surface,commit.record.title.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(text: &str) -> Vec<GridCell> { text.chars().map(|ch|GridCell {ch,..GridCell::blank()}).collect() }
    fn record(surface: &str) -> Record {
        Record {message_id:kasa_socket::tell::new_message_id(),address:Address {
            machine_id:"fixture-local-machine".into(),surface_key:surface.into(),surface_id:surface.into(),
            session_id:"session".into(),instance_id:"instance".into()},body:"hello".into(),
            body_hash:kasa_socket::tell::fingerprint("hello"),state:State::Accepted,reason:String::new(),
            accepted_at_ms:0,updated_at_ms:0,expires_at_ms:u64::MAX,reject_if_busy:false,receiver_agent_pid:1,title:String::new()}
    }

    #[test]
    fn multiline_draft_attachments_and_unknown_placeholder_never_count_as_empty() {
        use kasa_pty::AgentKind::Codex;
        assert!(!prompt_empty(&[row("›"),row("  second draft line")],0,Codex));
        assert!(!prompt_empty(&[row("›"),row("────────────────────"),row("draft beyond separator")],0,Codex));
        assert!(!prompt_empty(&[row("[Image #1]"),row("›")],1,Codex));
        assert!(!prompt_empty(&[row("› Find and fix a bug")],0,Codex));
        assert!(!prompt_empty(&[row("›"),row("unverified footer or draft")],0,Codex));
    }

    fn input_box(harness: kasa_pty::AgentKind, input: &[String], footer: &str) -> Vec<Vec<GridCell>> {
        let wide = |text: &str| { let mut row = row(text); row.resize(90,GridCell::blank()); row };
        let filled = |text: &str| {
            let mut row = wide(text);
            for cell in &mut row { cell.bg = kasa_bridge::screen::Color::Rgb(63,69,77); }
            row
        };
        let mut rows = vec![wide("Thinking… (esc to interrupt)")];
        match harness {
            kasa_pty::AgentKind::Claude => {
                rows.push(wide(&"─".repeat(90)));
                rows.extend(input.iter().map(|text|wide(text)));
                rows.push(wide(&"─".repeat(90)));
            }
            kasa_pty::AgentKind::Codex => {
                rows.push(filled(""));
                rows.extend(input.iter().map(|text|filled(text)));
                rows.push(filled(""));
            }
            _ => unreachable!(),
        }
        rows.push(wide(footer)); rows.push(wide("")); rows
    }

    #[test]
    fn real_input_boundaries_allow_footers_but_protect_identical_draft_text() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt,footer) in [
            (Claude,"❯","  bypass permissions on (shift+tab to cycle)"),
            (Codex,"›","  gpt-5.5 medium · kasaterm · main · Ask for approval · Context 3% used"),
        ] {
            let empty = input_box(harness,&[prompt.into()],footer);
            assert!(prompt_empty_at(&empty,2,2,harness));
            let draft = input_box(harness,&[prompt.into(),footer.into()],footer);
            assert!(!prompt_empty_at(&draft,2,2,harness));
            let multiline = input_box(harness,&[prompt.into(),"  first draft line".into(),"  second line".into()],footer);
            assert!(!prompt_empty_at(&multiline,2,2,harness));
            let attached = input_box(harness,&[prompt.into(),"  [Image #1]".into()],footer);
            assert!(!prompt_empty_at(&attached,2,2,harness));
            let mut pending_attachment = empty.clone();
            pending_attachment[0] = row("[Attachment: image.png]");
            assert!(!prompt_empty_at(&pending_attachment,2,2,harness));
            // Text-only slices lack a real box edge and cannot prove footer ownership.
            assert!(!prompt_empty_at(&[row(prompt),row(footer)],0,2,harness));
        }
    }

    #[test]
    fn styled_placeholder_requires_input_geometry_and_the_insertion_cursor() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt,hint,footer) in [
            (Claude,"❯","Try \"fix lint errors\"","  bypass permissions on (shift+tab to cycle)"),
            (Codex,"›","Run /review on my current changes","  gpt-5.5 medium · main · Context 3% used"),
        ] {
            let text = format!("{prompt} {hint}");
            let typed = input_box(harness,&[text.clone()],footer);
            assert!(!prompt_empty_at(&typed,2,2,harness));
            let mut placeholder = typed.clone();
            for cell in placeholder[2].iter_mut().skip(2) {
                cell.fg = kasa_bridge::screen::Color::Idx(8); cell.dim = true;
            }
            assert!(prompt_empty_at(&placeholder,2,2,harness));
            assert!(!prompt_empty_at(&placeholder,2,text.chars().count(),harness));
            let continuation = input_box(harness,&[prompt.into(),hint.into()],footer);
            assert!(!prompt_empty_at(&continuation,2,2,harness));
            let no_geometry = vec![placeholder[2].clone()];
            if harness == Claude { assert!(!prompt_empty_at(&no_geometry,0,2,harness)); }
        }
    }

    #[test]
    fn approval_and_question_options_inside_real_input_regions_remain_blocked() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt) in [(Claude,"❯"),(Codex,"›")] {
            for option in ["1. Yes","1. First option"] {
                let rows = input_box(harness,&[format!("{prompt} {option}"),"  2. Second option".into()],"status");
                assert!(!prompt_empty_at(&rows,2,2,harness));
                assert!(crate::input::rows_show_approval_prompt(&rows).is_some());
            }
        }
    }

    #[test]
    fn working_output_allows_empty_input_while_approval_and_question_are_blocked() {
        use kasa_pty::AgentKind::{Claude,Codex};
        for (harness,prompt) in [(Claude,"❯"),(Codex,"›")] {
            let cells = [row("Thinking... build running"),row(prompt)];
            assert!(prompt_empty(&cells,1,harness));
            assert!(crate::input::rows_show_approval_prompt(&cells).is_none());
            let cells = [row("Which option should be used?"),row(&format!("{prompt} 1. First"))];
            assert!(!prompt_empty(&cells,1,harness));
            assert!(crate::input::rows_show_approval_prompt(&cells).is_some());
        }
        assert!(!prompt_empty(&[row("$ ")],0,Claude));
    }

    #[test]
    fn eight_waiting_recipients_do_not_starve_the_ninth_ready_recipient() {
        let pending: Vec<_> = (0..9).map(|n|record(&format!("%{n:02}"))).collect();
        let mut scheduler = Scheduler::default();
        let mut seen = HashSet::new();
        for _ in 0..3 {
            for record in scheduler.select(pending.clone(),WORKERS) { seen.insert(record.address.surface_id); }
            scheduler.active.clear();
        }
        assert!(seen.contains("%08"));
        assert_eq!(seen.len(),9);
    }

    #[test]
    fn recipient_is_serialized_while_another_recipient_remains_eligible() {
        let mut scheduler = Scheduler::default();
        let selected = scheduler.select(vec![record("%1"),record("%1"),record("%2")],WORKERS);
        assert_eq!(selected.len(),2);
        assert!(scheduler.select(vec![record("%1"),record("%2")],WORKERS).is_empty());
        assert_eq!(scheduler.select(vec![record("%3")],WORKERS).len(),1);
    }

    #[derive(Clone)]
    struct Capture(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().push(bytes.to_vec()); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    fn fake_pty() -> (Arc<kasa_pty::PtySession>,Capture,crossbeam_channel::Sender<kasa_pty::ExtEvent>) {
        let (events,receiver) = crossbeam_channel::unbounded();
        let capture = Capture(Default::default());
        let session = kasa_pty::PtySession::start_external(kasa_pty::PtyOptions {
            pane_id:format!("tell-proof-{}",kasa_socket::tell::new_message_id()),cols:40,rows:8,..Default::default()
        },kasa_pty::ExternalIo {events:receiver,writer:Box::new(capture.clone()),on_resize:Arc::new(|_,_|{})}).unwrap();
        (Arc::new(session),capture,events)
    }

    #[test]
    fn deadline_worker_returns_evidence_off_thread_without_late_pty_input() {
        let (pty,capture,_events) = fake_pty();
        let caller = std::thread::current().id();
        let other = deadline_proof(Duration::from_secs(1),||Ok(std::thread::current().id())).unwrap();
        assert_ne!(caller,other);
        let revision = pty.input_revision();
        let observed = pty.clone();
        let late = deadline_proof(Duration::from_millis(10),move || {
            std::thread::sleep(Duration::from_millis(80)); Ok(observed.input_revision())
        });
        assert!(late.unwrap_err().contains("deadline"));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(pty.input_revision(),revision);
        assert!(capture.0.lock().unwrap().is_empty());
    }

    #[test]
    fn fake_pty_retains_multiline_and_pending_attachment_drafts() {
        let (pty,capture,_events) = fake_pty();
        pty.send_bytes(b"\r").unwrap();
        assert!(!pty.input_draft_present());
        pty.send_bytes(b"first\nsecond").unwrap();
        assert!(pty.input_draft_present());
        pty.send_bytes(b"\r").unwrap();
        let revision = pty.input_revision();
        pty.reserve_input_draft();
        assert!(pty.input_draft_present());
        assert!(pty.send_bytes_guarded(b"message",Some(revision)).is_err());
        assert_eq!(capture.0.lock().unwrap().as_slice(),[b"\r".to_vec(),b"first\nsecond".to_vec(),b"\r".to_vec()]);
    }

    /// 새 학생: 부팅 줄은 `send` 로 LF 로 끝나 초안 표시가 선 채 claude 가 뜬다. SessionStart 가
    /// 그 표시를 거둬야 첫 tell 이 들어간다 — 이 거둠을 빼면 첫 단언이 깨진다(한 번 빼 보고 확인했다).
    /// 부팅 줄 뒤에 사람이 친 글은 진짜 초안이라 SessionStart 가 와도 지키고, PTY 가 막 떠 아무것도
    /// 안 들어간 창(초기값 초안)도 빈 창으로 친다.
    /// 막 뜬 claude 는 붙여넣기를 늦게 그린다 — 입력이 그대로면 한도까지 기다리고, 누가 끼어들었거나
    /// 한도를 넘으면 보류한다. 기다림이 없으면 둘째 줄이 깨진다.
    #[test]
    fn slow_paste_echo_waits_but_interference_never_does() {
        let ms = Duration::from_millis;
        assert_eq!(commit_step(true,true,Some(ms(160))),CommitStep::Enter);
        assert_eq!(commit_step(true,false,Some(ms(160))),CommitStep::Wait);
        assert_eq!(commit_step(true,false,Some(ECHO_DEADLINE)),CommitStep::Withhold);
        assert_eq!(commit_step(false,false,Some(ms(160))),CommitStep::Withhold);
        assert_eq!(commit_step(false,true,Some(ms(160))),CommitStep::Withhold);
        assert_eq!(commit_step(true,false,None),CommitStep::Withhold);
    }

    /// 새 세션 화면: 입력창이 맨 위에 있고 아래 40줄이 비었다 — 바닥 30줄만 보던 에코 확인이
    /// 놓친 배치다. 넓은 글자의 빈칸(`\0`)도 글을 끊지 않는다.
    #[test]
    fn echo_is_read_from_the_input_box_wherever_it_sits() {
        let rule = row(&"─".repeat(40));
        let mut cells = vec![row("▐▛███▜▌ Claude Code"), row(""), rule.clone(),
            row("❯ ⟦프\0라\0나\0⟧ 검\0증\0용"), rule, row("  footer")];
        cells.extend(std::iter::repeat_with(||row("")).take(40));
        let text = input_box_text(&cells).expect("입력창");
        let compact: String = text.chars().filter(|c|!c.is_whitespace()).collect();
        assert!(compact.contains("⟦프라나⟧검증용"), "{compact:?}");
        assert!(!compact.contains("footer"), "입력창 밖은 안 본다");
        assert!(input_box_text(&[row("plain shell $")]).is_none(), "입력창이 없으면 옛 방식으로 넘긴다");
    }

    #[test]
    fn session_start_clears_the_boot_line_draft_but_keeps_later_typing() {
        let (pty,_capture,_events) = fake_pty();
        pty.send_bytes(b"claude --session-id x\n").unwrap();
        assert!(pty.input_draft_present(), "LF 로 끝난 부팅 줄은 제출로 안 친다");
        assert!(pty.agent_session_started());
        assert!(!pty.input_draft_present(), "새 세션의 입력창은 비어 있다");

        let (typed,_capture,_events) = fake_pty();
        typed.send_bytes(b"claude\n").unwrap();
        std::thread::sleep(Duration::from_millis(2));
        typed.send_bytes(b"half typed").unwrap();
        assert!(!typed.agent_session_started());
        assert!(typed.input_draft_present(), "부팅 줄 뒤에 친 글은 지킨다");

        let (fresh,_capture,_events) = fake_pty();
        assert!(fresh.input_draft_present());
        assert!(fresh.agent_session_started());
        assert!(!fresh.input_draft_present());
    }

    #[test]
    fn held_tells_are_counted_per_receiver_with_the_reason_people_can_act_on() {
        let mut first = record("%15"); first.reason = Hold::Draft.reason().into();
        let mut second = record("%15"); second.reason = Hold::Draft.reason().into();
        let mut fresh = record("%3"); fresh.reason = "stored; waiting for safe empty input".into();
        let mut approval = record("%4"); approval.reason = Hold::Approval.reason().into();
        let waiting = waiting_by_surface(&[first,second,fresh,approval]);
        assert_eq!(waiting.get("%15"), Some(&(2,Hold::Draft)));
        assert!(!waiting.contains_key("%3"), "한 번도 안 미뤄진 쪽지는 표시하지 않는다");
        assert_eq!(waiting_label(2,Hold::Draft), ["쪽지 2 대기 · 입력칸을 비우면 들어가요","쪽지 대기 · 입력칸 비우기"]);
        assert_eq!(waiting_label(1,Hold::Approval)[0], "쪽지 1 대기 · 승인·질문에 답하면 들어가요");
    }

    #[test]
    fn the_sender_hears_once_when_held_and_once_when_dropped() {
        let mut watch = Watch {
            id: "kt1.1.0123456789abcdef".into(), address: serde_json::json!({}), notify: "%25".into(),
            label: "아즈사@맥북".into(), first_line: "새 일 — 보드 걷기".into(),
            sent_at: Instant::now() - NOTICE_AFTER, warned: false, next_poll: Instant::now(),
        };
        let held = serde_json::json!({"state":"accepted","reason":Hold::Draft.reason(),"expires_at_ms":0});
        let WatchStep::Keep(Some(notice)) = watch_step(&mut watch,&held) else { panic!("2분 넘게 막히면 알린다") };
        assert!(notice.starts_with("[쪽지 대기] 아즈사@맥북") && notice.contains("쓰던 글") && notice.contains("보드 걷기"), "{notice}");
        assert!(matches!(watch_step(&mut watch,&held), WatchStep::Keep(None)), "대기 알림은 한 번만");
        let expired = serde_json::json!({"state":"failed","reason":"queued message expired"});
        let WatchStep::Drop(Some(gone)) = watch_step(&mut watch,&expired) else { panic!("버려지면 알린다") };
        assert!(gone.starts_with("[쪽지 못 감]") && gone.contains("만료"), "{gone}");
        let delivered = serde_json::json!({"state":"submitted","reason":""});
        assert!(matches!(watch_step(&mut watch,&delivered), WatchStep::Drop(None)), "들어가면 조용히 놓는다");
        watch.sent_at = Instant::now(); watch.warned = false;
        assert!(matches!(watch_step(&mut watch,&held), WatchStep::Keep(None)), "막 보낸 것은 아직 안 알린다");
    }
}
