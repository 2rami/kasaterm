//! 나쵸 판 입력칸을 받는 곳으로 보낸다 — 학생은 tell, 새 일은 디스패처, 나머지는 나쵸 대화.
//!
//! 받는 곳은 `route.rs` 가 정하고, 여기는 그 판정을 board 스냅샷의 pane 주소에 잇는다.
//! 판정이 서 있어도 보내기는 사람이 누를 때만 일어난다.

use super::route::{self, Candidate, RouteContext, RouteRequest, RouteTarget, StudentRef};
use super::*;
use serde_json::{json, Value};

/// 관문 판정 창구가 받는 학생 수 상한과 같다.
const MAX_CANDIDATES: usize = 24;
const SENT_KEEP: usize = 40;

pub(super) type RouteMail = Arc<Mutex<Vec<(RouteRequest, std::result::Result<Value, String>)>>>;

/// 이 판에서 보낸 지시 한 줄 — 대화에 「나 → 받는 곳」과 영수증으로 선다.
#[derive(Clone, Debug)]
pub(crate) struct SentNote {
    pub(crate) to: String,
    pub(crate) body: String,
    /// 영수증 문구. 보내는 중이면 None.
    pub(crate) receipt: Option<(bool, String)>,
}

/// 결과를 기다리는 보내기. 실패하면 같은 받는 곳·같은 글로 다시 누를 때 같은 id 를 쓴다.
pub(super) struct Outbox {
    id: String,
    target: RouteTarget,
    body: String,
    generation: u64,
    note: usize,
}

fn mirror(row: &OverviewPane) -> bool {
    row.status_reason.as_deref() == Some(crate::socket::REMOTE_MIRROR_REASON)
}

fn character(row: &OverviewPane) -> Option<&str> {
    row.character.as_deref().filter(|name| !name.is_empty())
}

fn route_problem(code: &str) -> String {
    match code {
        "not_found" => "관문이 아직 뜻 판정을 몰라요 — @이름·/새·/나쵸로 받는 곳을 정해 주세요",
        "key_required" => "나쵸 키가 없어 뜻 판정은 쉬어요 — @이름·/새·/나쵸로 정해 주세요",
        "signed_out" | "unauthorized" => "로그인하면 뜻 판정도 켜져요 — 지금은 @이름·/새·/나쵸로 정해 주세요",
        "rate_limited" => "판정이 잠시 쉬어요 — @이름·/새·/나쵸로 정해 주세요",
        _ => "판정 창구에 닿지 못했어요 — @이름·/새·/나쵸로 정해 주세요",
    }
    .into()
}

fn receipt_line(receipt: &Value) -> String {
    match receipt["state"].as_str().unwrap_or("") {
        "submitted" => "전달됨".into(),
        "accepted" => "접수됨 · 입력칸이 비면 들어가요".into(),
        "uncertain" => "전달 불확실 · 같은 글로 다시 누르면 같은 id 로 확인해요".into(),
        other if !other.is_empty() => other.into(),
        _ => "보냄".into(),
    }
}

pub(super) fn target_label(target: &RouteTarget) -> String {
    match target {
        RouteTarget::Student(student) => student.name.clone(),
        RouteTarget::NewTask => "새 일".into(),
        RouteTarget::Nacho => "나쵸".into(),
    }
}

impl Scene {
    fn route_rows(&self) -> impl Iterator<Item = &OverviewPane> {
        self.data.overview.panes.iter().filter(|row| {
            !mirror(row)
                && !row.detached
                && row.freshness == "fresh"
                && row.harness.as_deref().is_some_and(|harness| harness != "shell")
                && character(row).is_some()
        })
    }

    fn route_candidates(&self) -> Vec<Candidate> {
        let data = &self.data.overview;
        let local = |id: &str| data.sources.iter().any(|source| source.is_local && source.machine_id == id);
        let mut rows: Vec<&OverviewPane> = self.route_rows().collect();
        // 사람을 기다리는 학생이 먼저 — 상한에 걸려도 답할 사람이 빠지지 않게.
        rows.sort_by_key(|row| overview_status(row).1);
        rows.truncate(MAX_CANDIDATES);
        rows.into_iter()
            .map(|row| Candidate {
                student: StudentRef {
                    machine_id: row.address.machine_id.clone(),
                    machine_label: row.machine_label.clone(),
                    surface_id: row.address.surface_id.clone(),
                    name: character(row).unwrap_or_default().to_owned(),
                    local: local(&row.address.machine_id),
                },
                title: board_plain(&row.title, 80),
                latest: board_plain(row.done_summary.as_deref().filter(|s| !s.is_empty()).unwrap_or(&row.progress), 160),
                status: overview_status(row).0.to_owned(),
            })
            .collect()
    }

    /// 지금 보고 있는 pane 의 학생 — 「그거·다시」가 여기로 간다.
    fn route_selected(&self, candidates: &[Candidate]) -> Option<StudentRef> {
        let pane = self.target_pane.as_deref()?;
        candidates.iter().find(|c| c.student.local && c.student.surface_id == pane).map(|c| c.student.clone())
    }

    /// 입력칸 글이 바뀔 때. 확정된 글만 넘어온다 — IME 조합 중인 글자는 `draft` 밖에 있다.
    pub(crate) fn route_refresh(&mut self) {
        let candidates = self.route_candidates();
        let selected = self.route_selected(&candidates);
        let ctx = RouteContext { candidates: &candidates, selected: selected.as_ref(), last_sent: self.route_last_sent.as_ref() };
        self.route.update(&self.chat.draft, &ctx, Instant::now());
    }

    pub(crate) fn route_state(&self) -> &route::RouteState {
        self.route.state()
    }

    pub(crate) fn route_pending(&self) -> bool {
        self.route.pending()
    }

    pub(crate) fn route_choose(&mut self, target: RouteTarget) {
        self.route.choose(target);
    }

    pub(crate) fn sent_notes(&self) -> &[SentNote] {
        &self.route_sent
    }

    /// 판정 답을 싣고, 물을 때가 된 판정을 보내고, 다음 물을 때에 루프를 깨운다.
    pub(crate) fn route_tick(&mut self, proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> bool {
        let answers = std::mem::take(&mut *self.route_mail.lock().unwrap());
        let mut changed = !answers.is_empty();
        for (request, answer) in answers {
            self.route.answer(&request, answer);
        }
        let now = Instant::now();
        if let Some(request) = self.route.due(now) {
            changed = true;
            if board_fixture_requested() {
                self.route.answer(&request, Err("검증 화면에서는 뜻 판정을 부르지 않아요 — @이름·/새·/나쵸로 정해 주세요".into()));
            } else {
                let body = serde_json::to_vec(&route::request_body(&request.text, &request.candidates)).unwrap_or_default();
                let (mail, proxy) = (self.route_mail.clone(), proxy.clone());
                std::thread::spawn(move || {
                    let answer = super::assistant::route_blocking(body).map_err(route_problem);
                    mail.lock().unwrap().push((request, answer));
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
        }
        if let Some(wait) = self.route.next_wake(now) {
            let at = now + wait;
            if self.route_wake.is_none_or(|armed| armed <= now || armed > at) {
                self.route_wake = Some(at);
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(wait);
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
        }
        changed
    }

    fn route_address(&self, student: &StudentRef) -> Option<BoardAddress> {
        self.route_rows()
            .find(|row| row.address.machine_id == student.machine_id && row.address.surface_id == student.surface_id)
            .map(|row| row.address.clone())
    }

    fn push_note(&mut self, to: String, body: String) -> usize {
        if self.route_sent.len() >= SENT_KEEP {
            self.route_sent.remove(0);
            if let Some(outbox) = self.route_outbox.as_mut() {
                outbox.note = outbox.note.saturating_sub(1);
            }
        }
        self.route_sent.push(SentNote { to, body, receipt: None });
        self.route_sent.len() - 1
    }

    /// 보내기를 눌렀다. 학생·새 일이면 일꾼에게 맡기고 `true`, 나쵸 대화로 가야 하면 `false`.
    pub(crate) fn route_send(
        &mut self,
        backend: Arc<dyn Backend>,
        proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    ) -> bool {
        let Some(target) = self.route.state().target().cloned() else { return false };
        if matches!(target, RouteTarget::Nacho) {
            return false;
        }
        if board_fixture_requested() {
            self.toast = Some((false, "검증 화면에서는 학생에게 보내지 않아요".into(), Instant::now()));
            return true;
        }
        if self.route_outbox.as_ref().is_some_and(|outbox| outbox.generation > self.applied_action_generation) {
            return true;
        }
        let (body, title) = route::outgoing(&self.chat.draft);
        if body.is_empty() {
            self.toast = Some((false, "보낼 글이 비었어요".into(), Instant::now()));
            return true;
        }
        let action = match &target {
            RouteTarget::Student(student) => {
                let Some(address) = self.route_address(student) else {
                    self.toast = Some((false, format!("{} 의 창을 판에서 다시 찾지 못했어요 — 새로고침 뒤 다시 보내 주세요", student.name), Instant::now()));
                    return true;
                };
                // 같은 받는 곳에 같은 글을 다시 누르면 같은 id — 불확실한 전달을 두 번 넣지 않는다.
                let id = match &self.route_outbox {
                    Some(outbox) if outbox.target == target && outbox.body == body => outbox.id.clone(),
                    _ => format!("kt1.{}.{}", kasa_socket::tell::now_ms(), uuid::Uuid::new_v4().simple()),
                };
                let mut params = json!({"address": address, "message_id": id, "body": body});
                if let Some(title) = title {
                    params["title"] = json!(title);
                }
                WorkerAction::Tell(params)
            }
            RouteTarget::NewTask => WorkerAction::Dispatch(body.clone()),
            RouteTarget::Nacho => unreachable!(),
        };
        let id = match &action {
            WorkerAction::Tell(params) => params["message_id"].as_str().unwrap_or_default().to_owned(),
            _ => String::new(),
        };
        let note = self.push_note(target_label(&target), body.clone());
        self.run_action(backend, action, proxy);
        self.route_outbox = Some(Outbox { id, target, body, generation: self.action_generation, note });
        true
    }

    /// 일꾼의 보내기 결과. 성공이면 입력칸을 비우고 받는 곳을 기억한다.
    pub(super) fn route_settle(&mut self, generation: u64, ok: bool, message: &str) {
        let Some(outbox) = self.route_outbox.as_ref().filter(|outbox| outbox.generation == generation) else { return };
        if let Some(note) = self.route_sent.get_mut(outbox.note) {
            note.receipt = Some((ok, message.to_owned()));
        }
        if !ok {
            return;
        }
        let outbox = self.route_outbox.take().unwrap();
        if route::outgoing(&self.chat.draft).0 == outbox.body {
            self.chat.draft.clear();
            self.caret = 0;
        }
        if let RouteTarget::Student(student) = outbox.target {
            self.route_last_sent = Some(student);
        }
        self.route.clear();
    }
}

pub(super) fn execute_tell(backend: &Arc<dyn Backend>, params: Value) -> anyhow::Result<String> {
    let receipt = backend.collab_tell(&params)?;
    Ok(receipt_line(&receipt))
}

pub(super) fn execute_dispatch(backend: &Arc<dyn Backend>, instruction: String) -> anyhow::Result<String> {
    let ids = kasa_mcp::dispatch::plan_and_push_blocking(&instruction, backend)?;
    anyhow::ensure!(!ids.is_empty(), "새 일을 대기열에 넣지 못했어요");
    Ok(if kasa_mcp::dispatch::read_config().enabled {
        format!("새 일 {}건 · 맡을 학생을 찾는 중", ids.len())
    } else {
        format!("새 일 {}건 · 자동 배정이 꺼져 있어 대기열에 있어요", ids.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_read_as_plain_states() {
        assert_eq!(receipt_line(&json!({"state":"submitted"})), "전달됨");
        assert!(receipt_line(&json!({"state":"accepted"})).starts_with("접수됨"));
        assert!(receipt_line(&json!({"state":"uncertain"})).contains("같은 id"));
        assert_eq!(route_problem("not_found").contains("@이름"), true);
    }
}
