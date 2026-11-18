//! 학생·나쵸가 낸 PR 요청을 사람이 승인하는 확인 창(docs/account-connections.md 「쓰기는 사람 확인을 거친다」).
//! 설정에는 그리지 않는다. 1분마다 관문의 대기 쓰기를 받아 새 요청을 알리고, 앱이 앞에 있으면(또는 알림을 누르면)
//! 내용 전부를 보인 결정 시트를 하나씩 띄운다. 승인은 그 시트의 주 단추에서만 나고, 시트가 보인 digest 에 묶인다.

use super::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

const REFRESH: std::time::Duration = std::time::Duration::from_secs(60);
/// 알림을 누르면 이 요청의 시트를 연다 — 알림 식별자의 칸 자리에 싣는다(`chrome::notify_ident`).
pub(crate) const ACTION: &str = "__kasaterm_pr_request__";
/// 만든 PR 알림을 누르면 그 주소를 연다.
pub(crate) const OPEN_PR: &str = "__kasaterm_pr_open__";
/// 「나중에」로 닫은 요청을 다시 띄우기까지. 앱이 앞에 있을 때 다시 뜬다(관문은 24시간 쥔다).
const LATER_FOR: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// 한 번에 이보다 많이 오면 알림을 하나로 묶는다.
const NOTICE_EACH: usize = 3;

const TITLE: &str = "PR 을 올려도 될까요?";
/// [주 동작, 기본 동작(Return), 왼쪽 끝].
const BUTTONS: [&str; 3] = ["PR 만들기", "나중에", "버리기"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Create,
    Later,
    Discard,
}

impl Choice {
    fn from_button(index: Option<usize>) -> Self {
        match index {
            Some(0) => Choice::Create,
            Some(2) => Choice::Discard,
            _ => Choice::Later,
        }
    }
}

/// 시트의 답과 그 시트를 띄울 때 만든 표 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static ANSWER: Mutex<Option<(u64, Choice)>> = Mutex::new(None);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    Approve { id: String, digest: String },
    Reject { id: String },
}

impl Effect {
    fn id(&self) -> &str {
        match self {
            Effect::Approve { id, .. } | Effect::Reject { id } => id,
        }
    }
}

#[derive(Debug)]
enum Done {
    Listed(Result<serde_json::Value, String>),
    Acted(Effect, Result<serde_json::Value, String>),
}

#[derive(Debug, PartialEq, Eq)]
struct Notice {
    title: String,
    body: String,
    dedup: Option<String>,
    route: Option<(&'static str, String)>,
}

/// 띄운 시트. 답은 표(`ticket`)가 맞을 때만 받는다 — 거둔 시트·앞 시트의 답이 다른 요청에 붙지 않게.
struct Shown {
    ticket: u64,
    id: String,
    digest: String,
    handle: usize,
    opened: std::time::Instant,
}

#[derive(Default)]
pub(crate) struct State {
    pending: Vec<serde_json::Value>,
    install_url: Option<String>,
    fetched: Option<std::time::Instant>,
    fetch: Option<Receiver<Done>>,
    busy: Option<Receiver<Done>>,
    shown: Option<Shown>,
    /// 띄울 차례. 알림을 누른 요청은 맨 앞으로 온다.
    queue: VecDeque<String>,
    /// 이 실행에서 이미 알렸거나 차례에 넣은 요청.
    seen: HashSet<String>,
    /// 「나중에」로 닫은 요청과 다시 띄울 때 — 그 전에는 알림을 눌러야 뜬다.
    later: HashMap<String, std::time::Instant>,
    /// 다른 곳에서 끝나 거둘 시트.
    stale: Option<usize>,
    tickets: u64,
    /// 검증 실행의 가짜 목록 — 관문에 묻지 않고, 만들기·버리기도 여기서 끝난다.
    fixture: bool,
    /// 검증 실행이 찍고 누른 시트 수.
    rig_step: usize,
}

fn readable(error: &str) -> String {
    let code = error.split(':').next().unwrap_or(error);
    match code {
        "approver_required" => "이 화면에서 승인 자격을 다시 받지 못했어요. 잠시 뒤 다시 눌러 주세요".into(),
        "content_changed" => "보는 동안 내용이 바뀌었어요. 알림을 눌러 바뀐 내용을 다시 확인해 주세요".into(),
        "reconnect_required" => "GitHub 연결이 풀렸어요. 설정 → 계정 → 로그인 방법에서 GitHub 을 다시 연결해 주세요".into(),
        "feature_missing" => "이 연결에 PR 권한이 없어요. 설정 → 계정 → 로그인 방법에서 GitHub 을 다시 연결해 주세요".into(),
        "repo_not_accessible" => "그 레포에 GitHub 앱이 설치되지 않았어요. 열린 GitHub 화면에서 레포를 고른 뒤 이 알림을 눌러 다시 승인해 주세요".into(),
        "provider_rejected" => format!(
            "GitHub 이 거절했어요 — {}",
            error.split_once(": ").map_or("", |(_, detail)| detail)
        ),
        "result_unknown" => "보냈지만 결과를 못 받았어요. GitHub 에서 확인해 주세요".into(),
        "setup_required" => "관문에 GitHub 연결이 아직 준비되지 않았어요".into(),
        "update_required" => "관문이 이 기능을 아직 몰라요. 관문 업데이트가 필요해요".into(),
        "not_found" => "이미 처리됐거나 사라진 요청이에요".into(),
        "gateway_unreachable" => "관문에 닿지 못했어요. 연결 상태를 확인해 주세요".into(),
        _ => "요청을 마치지 못했어요. 다시 시도해 주세요".into(),
    }
}

fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}

/// 알림 한 줄 — 레포·제목과 브랜치, 요청한 곳.
fn summary(pending: &serde_json::Value) -> String {
    let write = &pending["write"];
    format!(
        "{} · {}\n{} → {} · {}",
        text(write, "repo"),
        text(write, "title"),
        text(write, "head"),
        text(write, "base"),
        text(pending, "device_label"),
    )
}

fn known(pending: &serde_json::Value) -> bool {
    pending["write"]["kind"] == "pr"
}

/// 시트의 안내와 원문 칸 — 사람이 승인하는 것 전부(계정·레포·브랜치·제목·본문·요청한 곳)를 그대로 싣는다.
fn sheet_text(pending: &serde_json::Value, more: usize) -> (String, Option<String>) {
    let write = &pending["write"];
    let mut info = if known(pending) {
        format!(
            "보낼 계정  GitHub · {}\n레포  {}\n브랜치  {} → {}{}\n제목  {}\n요청한 곳  {}",
            text(pending, "display"),
            text(write, "repo"),
            text(write, "head"),
            text(write, "base"),
            if write["draft"] == true { " (초안)" } else { "" },
            text(write, "title"),
            text(pending, "device_label"),
        )
    } else {
        format!("이 앱이 모르는 요청이에요. 앱을 업데이트해 주세요.\n요청한 곳  {}", text(pending, "device_label"))
    };
    let body = text(write, "body");
    if known(pending) && body.is_empty() {
        info.push_str("\n본문  없음");
    }
    if more > 0 {
        info.push_str(&format!("\n\n기다리는 요청이 {more}개 더 있어요"));
    }
    (info, (known(pending) && !body.is_empty()).then(|| body.to_string()))
}

impl State {
    fn find(&self, id: &str) -> Option<&serde_json::Value> {
        self.pending.iter().find(|pending| pending["id"] == id)
    }

    fn refresh_due(&mut self, force: bool, wake: Option<EventLoopProxy<UserEvent>>) {
        if self.fetch.is_some() || self.fixture {
            return;
        }
        if !force && self.fetched.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        self.fetched = Some(std::time::Instant::now());
        let (tx, rx) = mpsc::channel();
        self.fetch = Some(rx);
        std::thread::spawn(move || {
            let result = kasa_device::device_auth::handle(&serde_json::json!({"op":"connections"}))
                .map_err(|error| error.to_string());
            let _ = tx.send(Done::Listed(result));
            if let Some(wake) = wake {
                let _ = wake.send_event(UserEvent::Redraw);
            }
        });
    }

    /// A new connection was just made from the account page; look for requests without waiting a minute.
    pub(crate) fn refresh_now(&mut self) {
        self.refresh_due(true, None);
    }

    /// 새 목록. 처음 보는 요청은 차례에 넣고, 앱이 뒤에 있으면 알림으로 알린다. 띄운 시트의 요청이 끝났거나
    /// 내용이 바뀌었으면 그 시트를 거둔다(바뀐 것은 새 내용으로 다시 띄운다).
    fn listed(&mut self, data: &serde_json::Value, front: bool) -> Vec<Notice> {
        self.pending = data["pending"].as_array().cloned().unwrap_or_default();
        self.install_url = data["github_install_url"].as_str().map(str::to_string);
        let live: HashSet<String> =
            self.pending.iter().filter_map(|p| p["id"].as_str().map(str::to_string)).collect();
        self.queue.retain(|id| live.contains(id));
        self.later.retain(|id, _| live.contains(id));
        if let Some(shown) = &self.shown {
            let digest = self.find(&shown.id).map(|p| text(p, "digest").to_string());
            if digest.as_deref() != Some(shown.digest.as_str()) {
                if digest.is_some() {
                    self.queue.push_front(shown.id.clone());
                }
                self.stale = Some(shown.handle);
                self.shown = None;
            }
        }
        let fresh: Vec<serde_json::Value> = self
            .pending
            .iter()
            .filter(|p| p["id"].as_str().is_some_and(|id| !self.seen.contains(id)))
            .cloned()
            .collect();
        for pending in &fresh {
            let id = text(pending, "id").to_string();
            self.seen.insert(id.clone());
            self.queue.push_back(id);
        }
        if front || fresh.is_empty() {
            return Vec::new();
        }
        if fresh.len() > NOTICE_EACH {
            return vec![Notice {
                title: format!("PR 요청 {}개가 기다려요", fresh.len()),
                body: "누르면 하나씩 보여 드려요".into(),
                dedup: None,
                route: Some((ACTION, text(&fresh[0], "id").to_string())),
            }];
        }
        fresh
            .iter()
            .map(|pending| Notice {
                title: TITLE.into(),
                body: summary(pending),
                dedup: Some(format!("work-pending:{}", text(pending, "id"))),
                route: Some((ACTION, text(pending, "id").to_string())),
            })
            .collect()
    }

    /// 다음에 띄울 요청. 차례가 비었거나 시트·처리가 이미 있으면 없다.
    fn next(&mut self) -> Option<serde_json::Value> {
        if self.shown.is_some() || self.busy.is_some() {
            return None;
        }
        let now = std::time::Instant::now();
        let due: Vec<String> = self.later.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()).collect();
        for id in due {
            self.later.remove(&id);
            self.queue.push_back(id);
        }
        while let Some(id) = self.queue.pop_front() {
            if self.later.contains_key(&id) {
                continue;
            }
            if let Some(pending) = self.find(&id) {
                return Some(pending.clone());
            }
        }
        None
    }

    /// 띄운 요청 말고 기다리는 수(「나중에」 것 포함).
    fn others(&self, id: &str) -> usize {
        self.pending.iter().filter(|p| p["id"] != id).count()
    }

    fn opened(&mut self, pending: &serde_json::Value, ticket: u64, handle: Option<usize>) {
        let id = text(pending, "id").to_string();
        match handle {
            Some(handle) => {
                self.shown = Some(Shown {
                    ticket,
                    id,
                    digest: text(pending, "digest").to_string(),
                    handle,
                    opened: std::time::Instant::now(),
                })
            }
            // 시트를 못 띄우는 자리면 틱마다 다시 시도하지 않는다.
            None => {
                self.later.insert(id, std::time::Instant::now() + LATER_FOR);
            }
        }
    }

    /// 시트의 답. 그 시트가 보인 요청·digest 그대로만 승인하고, 표가 다르면 버린다.
    fn answered(&mut self, ticket: u64, choice: Choice) -> Option<Effect> {
        if self.shown.as_ref().is_none_or(|shown| shown.ticket != ticket) {
            return None;
        }
        let shown = self.shown.take()?;
        match choice {
            Choice::Create => Some(Effect::Approve { id: shown.id, digest: shown.digest }),
            Choice::Discard => Some(Effect::Reject { id: shown.id }),
            Choice::Later => {
                self.later.insert(shown.id, std::time::Instant::now() + LATER_FOR);
                None
            }
        }
    }

    /// 알림을 눌렀다 — 「나중에」로 닫았던 것도 다시 띄운다.
    fn clicked(&mut self, id: &str) {
        if self.find(id).is_none() || self.shown.as_ref().is_some_and(|shown| shown.id == id) {
            return;
        }
        self.later.remove(id);
        self.queue.retain(|queued| queued != id);
        self.queue.push_front(id.to_string());
    }

    fn run(&mut self, effect: Effect, wake: EventLoopProxy<UserEvent>) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        let fixture = self.fixture;
        std::thread::spawn(move || {
            let result = if fixture {
                Ok(serde_json::json!({"status":"created","number":1,"url":"https://github.com/2rami/kasaterm/pull/1"}))
            } else {
                match &effect {
                    Effect::Approve { id, digest } => kasa_device::device_auth::connections::approve(id, digest),
                    Effect::Reject { id } => {
                        kasa_device::device_auth::handle(&serde_json::json!({"op":"reject","id":id}))
                    }
                }
                .map_err(|error| error.to_string())
            };
            let _ = tx.send(Done::Acted(effect, result));
            let _ = wake.send_event(UserEvent::Redraw);
        });
    }

    /// 끝난 처리의 알림과, 열 GitHub 설치 화면.
    fn acted(&mut self, effect: &Effect, result: Result<serde_json::Value, String>) -> (Vec<Notice>, Option<String>) {
        self.fetched = None;
        let id = effect.id().to_string();
        let item = self.find(&id).cloned().unwrap_or_default();
        let again = Some((ACTION, id.clone()));
        match (effect, result) {
            (Effect::Approve { .. }, Ok(value)) => {
                self.pending.retain(|p| p["id"] != id.as_str());
                let url = text(&value, "url");
                let notice = Notice {
                    title: format!("PR #{} 을 만들었어요", value["number"]),
                    body: format!("{} · {}", text(&item["write"], "repo"), text(&item["write"], "title")),
                    dedup: None,
                    route: url.starts_with("https://github.com/").then(|| (OPEN_PR, url.to_string())),
                };
                (vec![notice], None)
            }
            (Effect::Reject { .. }, Ok(_)) => {
                self.pending.retain(|p| p["id"] != id.as_str());
                (Vec::new(), None)
            }
            (Effect::Approve { .. }, Err(error)) => {
                let install = error.starts_with("repo_not_accessible").then(|| self.install_url.clone()).flatten();
                (vec![Notice { title: "PR 을 만들지 못했어요".into(), body: readable(&error), dedup: None, route: again }], install)
            }
            (Effect::Reject { .. }, Err(error)) => {
                (vec![Notice { title: "요청을 버리지 못했어요".into(), body: readable(&error), dedup: None, route: again }], None)
            }
        }
    }

    /// 검증 실행 화면: 본문이 긴 요청과 본문 없는 초안 요청. 관문에 묻지 않는다.
    pub(crate) fn fixture(&mut self) {
        self.fixture = true;
        let body: String = (1..=24).map(|n| format!("- 바꾼 것 {n}: 설정 「계정」 정리\n")).collect();
        let data = serde_json::json!({
            "github_install_url":"https://github.com/apps/kasa-work/installations/new",
            "pending":[
                {"id":"pw_a","digest":"d","provider":"github","display":"octo","device_label":"내 MacBook Pro · 케이",
                 "write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/login-first","title":"로그인 첫 화면 정리",
                 "body":format!("이번 판에서 한 일\n\n{body}\n폰 화면은 다음 판에 붙입니다."),"draft":false}},
                {"id":"pw_b","digest":"d","provider":"github","display":"octo","device_label":"맥미니 · 나쵸",
                 "write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/work","title":"일 권한 화면","body":"","draft":true}}]});
        self.listed(&data, true);
    }
}

fn post(notice: &Notice) {
    crate::chrome::notify_desktop(
        &notice.title,
        &notice.body,
        None,
        notice.dedup.as_deref(),
        notice.route.as_ref().map(|(action, key)| (*action, Some(key.as_str()))),
    );
}

impl App {
    /// 목록 받기·알림·시트 답·끝난 처리, 그리고 앱이 앞이면 다음 시트. 설정을 안 열어도 돈다 — 로그인 여부는 뒷실
    /// 조회가 가린다(로그아웃·격리 실행이면 관문에 닿지 않고 빈 손으로 돌아온다).
    pub(crate) fn pr_requests_poll(&mut self) {
        if crate::lite_mode() {
            return;
        }
        let work = &mut self.device_account.work;
        // 검증 리그는 사람 화면의 초점을 안 뺏으려 뒤에 떠 있을 수 있다 — 가짜 목록이면 앞에 있는 셈 친다.
        let front = self.window_focused || work.fixture;
        work.refresh_due(false, Some(self.proxy.clone()));
        let mut notices = Vec::new();
        let mut install = None;
        if let Some(Ok(Done::Listed(result))) = work.fetch.as_ref().map(Receiver::try_recv) {
            work.fetch = None;
            match result {
                Ok(data) => notices = work.listed(&data, front),
                Err(error) if error.contains("signed_out") || error.contains("isolated_run") => {
                    work.listed(&serde_json::Value::Null, front);
                }
                Err(_) => {}
            }
        }
        if let Some(handle) = work.stale.take() {
            super::op::close_sheet(handle);
        }
        if let Some((ticket, choice)) = ANSWER.lock().ok().and_then(|mut answer| answer.take()) {
            if let Some(effect) = work.answered(ticket, choice) {
                work.run(effect, self.proxy.clone());
            }
        }
        if let Some(Ok(Done::Acted(effect, result))) = work.busy.as_ref().map(Receiver::try_recv) {
            work.busy = None;
            let (done, url) = work.acted(&effect, result);
            notices.extend(done);
            install = url;
        }
        for notice in &notices {
            post(notice);
        }
        if let Some(url) = install {
            crate::chrome::open_url_in_browser(&url);
        }
        self.pr_sheet_rig();
        if front {
            self.pr_show_next();
        }
    }

    fn pr_show_next(&mut self) {
        let Some(window) = self.window.as_ref() else { return };
        let work = &mut self.device_account.work;
        let Some(pending) = work.next() else { return };
        work.tickets += 1;
        let ticket = work.tickets;
        let (info, body) = sheet_text(&pending, work.others(text(&pending, "id")));
        let spec = super::op::SheetSpec {
            title: TITLE,
            info: &info,
            body: body.as_deref(),
            buttons: &BUTTONS,
            primary_enabled: known(&pending),
        };
        let handle = super::op::decision_sheet(window, &spec, move |index| {
            if let Ok(mut answer) = ANSWER.lock() {
                *answer = Some((ticket, Choice::from_button(index)));
            }
        });
        work.opened(&pending, ticket, handle);
    }

    /// PR 요청 알림을 눌렀다.
    pub(crate) fn pr_request_clicked(&mut self, id: &str) {
        if let Some(window) = self.window.as_ref() {
            window.focus_window();
        }
        self.device_account.work.clicked(id);
        self.pr_show_next();
    }

    /// 검증 실행이 띄운 시트를 정한 시각에 찍고 `KASATERM_PR_SHEET_PRESS` 의 차례대로 누른다 — 사람 누름과 같은
    /// 완료 처리기를 탄다. 찍는 경로의 `{n}` 은 몇 번째 시트인지로 바뀐다.
    fn pr_sheet_rig(&mut self) {
        if !crate::verification_run() {
            return;
        }
        let Some(ms) = std::env::var("KASATERM_PR_SHEET_MS").ok().and_then(|v| v.parse::<u64>().ok()) else { return };
        let work = &mut self.device_account.work;
        let Some(shown) = &work.shown else { return };
        if shown.opened.elapsed() < std::time::Duration::from_millis(ms) || work.rig_step == usize::MAX {
            return;
        }
        let (handle, step) = (shown.handle, work.rig_step);
        if let Ok(path) = std::env::var("KASATERM_PR_SHEET_SHOT") {
            super::op::shot_alert(handle, &path.replace("{n}", &step.to_string()));
        }
        let press = std::env::var("KASATERM_PR_SHEET_PRESS").unwrap_or_default();
        match press.split(',').nth(step).and_then(|v| v.trim().parse::<usize>().ok()) {
            Some(index) => {
                work.rig_step += 1;
                super::op::press_sheet(handle, index);
            }
            None => work.rig_step = usize::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, digest: &str) -> serde_json::Value {
        serde_json::json!({"id":id,"digest":digest,"provider":"github","display":"octo",
            "device_label":"Laptop · 케이","write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/x",
            "title":"주간 정리","body":"한 줄\n  들여 쓴 둘째 줄\n\n`셋째` 줄 <b>그대로</b>","draft":true}})
    }

    fn list(items: &[serde_json::Value]) -> serde_json::Value {
        serde_json::json!({"pending":items,"github_install_url":"https://github.com/apps/kasa-work/installations/new"})
    }

    /// 다음 시트를 띄운 척 — 표를 돌려준다.
    fn show(state: &mut State) -> (u64, serde_json::Value) {
        let item = state.next().expect("nothing to show");
        state.tickets += 1;
        let ticket = state.tickets;
        state.opened(&item, ticket, Some(7));
        (ticket, item)
    }

    #[test]
    fn the_sheet_shows_the_request_word_for_word() {
        let item = pending("pw_a", "d");
        let (info, body) = sheet_text(&item, 0);
        assert_eq!(body.as_deref(), Some("한 줄\n  들여 쓴 둘째 줄\n\n`셋째` 줄 <b>그대로</b>"));
        for line in ["보낼 계정  GitHub · octo", "레포  2rami/kasaterm", "브랜치  feat/x → main (초안)", "제목  주간 정리", "요청한 곳  Laptop · 케이"] {
            assert!(info.lines().any(|l| l == line), "missing {line:?} in {info:?}");
        }
        let mut empty = pending("pw_b", "d");
        empty["write"]["body"] = "".into();
        let (info, body) = sheet_text(&empty, 2);
        assert!(body.is_none());
        assert!(info.contains("본문  없음") && info.ends_with("기다리는 요청이 2개 더 있어요"));
        let mut odd = pending("pw_c", "d");
        odd["write"]["kind"] = "mail".into();
        assert!(!known(&odd) && sheet_text(&odd, 0).1.is_none(), "an unknown write offered something to approve");
    }

    #[test]
    fn create_approves_exactly_what_the_sheet_showed() {
        let mut state = State::default();
        state.listed(&list(&[pending("pw_a", "d1"), pending("pw_b", "d2")]), true);
        let (ticket, item) = show(&mut state);
        assert_eq!(item["id"], "pw_a");
        assert!(state.next().is_none(), "two sheets at once");
        assert_eq!(state.answered(ticket, Choice::Create), Some(Effect::Approve { id: "pw_a".into(), digest: "d1".into() }));
        assert!(state.shown.is_none());
        let (notices, install) = state.acted(
            &Effect::Approve { id: "pw_a".into(), digest: "d1".into() },
            Ok(serde_json::json!({"status":"created","number":12,"url":"https://github.com/2rami/kasaterm/pull/12"})),
        );
        assert!(install.is_none());
        assert_eq!(notices[0].title, "PR #12 을 만들었어요");
        assert_eq!(notices[0].route, Some((OPEN_PR, "https://github.com/2rami/kasaterm/pull/12".into())));
        assert_eq!(show(&mut state).1["id"], "pw_b", "the next request did not follow");
    }

    #[test]
    fn discard_rejects_and_later_keeps_it_waiting() {
        let mut state = State::default();
        state.listed(&list(&[pending("pw_a", "d"), pending("pw_b", "d")]), true);
        let (ticket, _) = show(&mut state);
        assert_eq!(state.answered(ticket, Choice::Discard), Some(Effect::Reject { id: "pw_a".into() }));
        assert_eq!(state.acted(&Effect::Reject { id: "pw_a".into() }, Ok(serde_json::json!({"ok":true}))).0, Vec::new());
        let (ticket, item) = show(&mut state);
        assert_eq!(item["id"], "pw_b");
        assert_eq!(state.answered(ticket, Choice::Later), None, "later sent something");
        assert!(state.next().is_none(), "a request closed for later came back by itself");
        state.listed(&list(&[pending("pw_b", "d")]), true);
        assert!(state.next().is_none(), "a refresh brought a later request back");
        state.clicked("pw_b");
        let (ticket, item) = show(&mut state);
        assert_eq!(item["id"], "pw_b", "clicking its notification did not reopen it");
        state.answered(ticket, Choice::Later);
        state.later.insert("pw_b".into(), std::time::Instant::now());
        assert_eq!(show(&mut state).1["id"], "pw_b", "a later request never came back");
    }

    #[test]
    fn nothing_approves_without_the_shown_sheet() {
        let mut state = State::default();
        state.listed(&list(&[pending("pw_a", "d")]), true);
        assert_eq!(state.answered(1, Choice::Create), None, "an answer with no sheet open approved");
        state.clicked("pw_a");
        let (ticket, _) = show(&mut state);
        assert_eq!(state.answered(ticket + 1, Choice::Create), None, "another sheet's answer approved this one");
        assert!(state.shown.is_some());
        assert!(state.answered(ticket, Choice::Create).is_some());
        assert_eq!(state.answered(ticket, Choice::Create), None, "one sheet approved twice");
    }

    #[test]
    fn a_request_handled_elsewhere_or_changed_closes_its_sheet() {
        let mut state = State::default();
        state.listed(&list(&[pending("pw_a", "d1"), pending("pw_b", "d")]), true);
        let (ticket, _) = show(&mut state);
        state.listed(&list(&[pending("pw_a", "d2"), pending("pw_b", "d")]), true);
        assert_eq!(state.stale, Some(7));
        assert_eq!(state.answered(ticket, Choice::Create), None, "the closed sheet's answer approved the changed request");
        let (_, item) = show(&mut state);
        assert_eq!((item["id"].as_str(), item["digest"].as_str()), (Some("pw_a"), Some("d2")));
        state.listed(&list(&[pending("pw_b", "d")]), true);
        assert!(state.shown.is_none() && state.stale.is_some());
        assert_eq!(show(&mut state).1["id"], "pw_b");
    }

    #[test]
    fn requests_notify_once_when_the_app_is_behind() {
        let mut state = State::default();
        let notices = state.listed(&list(&[pending("pw_a", "d")]), false);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].body, "2rami/kasaterm · 주간 정리\nfeat/x → main · Laptop · 케이");
        assert_eq!(notices[0].route, Some((ACTION, "pw_a".into())));
        assert!(state.listed(&list(&[pending("pw_a", "d")]), false).is_empty(), "the same request notified twice");
        assert!(state.listed(&list(&[pending("pw_a", "d"), pending("pw_b", "d")]), true).is_empty(), "notified while in front");
        let many: Vec<_> = (0..5).map(|n| pending(&format!("pw_{n}"), "d")).collect();
        let notices = State::default().listed(&list(&many), false);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].title, "PR 요청 5개가 기다려요");
    }

    #[test]
    fn a_failed_approval_points_back_to_the_request() {
        let mut state = State::default();
        state.listed(&list(&[pending("pw_a", "d")]), true);
        let effect = Effect::Approve { id: "pw_a".into(), digest: "d".into() };
        let (notices, install) = state.acted(&effect, Err("repo_not_accessible".into()));
        assert_eq!(install.as_deref(), Some("https://github.com/apps/kasa-work/installations/new"));
        assert_eq!(notices[0].route, Some((ACTION, "pw_a".into())));
        assert!(state.find("pw_a").is_some(), "a request that went back to waiting was dropped");
        assert!(readable("provider_rejected: Validation Failed").ends_with("Validation Failed"));
    }
}
