//! 설정 「계정」의 「일 권한」 — 이 계정에 붙인 GitHub 연결과, 학생·나쵸가 낸 PR 이 사람 승인을
//! 기다리는 목록(docs/account-connections.md). 토큰은 관문에만 있고 여기엔 목록뿐이다.

use super::*;
use std::collections::HashSet;

const REFRESH: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_SHOWN_LINES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    InstallGithub,
    AskDisconnect(usize),
    Disconnect,
    KeepConnection,
    Open(usize),
    Close,
    Approve,
    Reject,
}

#[derive(Debug)]
enum Done {
    Listed(Result<serde_json::Value, String>),
    Acted(Result<String, String>),
}

#[derive(Default)]
pub(crate) struct State {
    data: serde_json::Value,
    fetched: Option<std::time::Instant>,
    fetch: Option<Receiver<Done>>,
    busy: Option<Receiver<Done>>,
    /// (id, digest) of the pending write the person opened. Approval acts on exactly this.
    open: Option<(String, String)>,
    disconnect: Option<String>,
    message: Option<(String, bool)>,
    notified: HashSet<String>,
    /// Verification-run screen with a made-up list; no gateway is asked.
    fixture: bool,
}

#[derive(Clone, Default)]
pub(crate) struct View {
    signed_in: bool,
    data: serde_json::Value,
    open: Option<String>,
    disconnect: Option<String>,
    busy: bool,
    message: Option<(String, bool)>,
}

fn readable(error: &str) -> String {
    let code = error.split(':').next().unwrap_or(error);
    match code {
        "approver_required" => "이 화면에서 승인 자격을 다시 받지 못했어요. 잠시 뒤 다시 눌러 주세요".into(),
        "content_changed" => "보는 동안 내용이 바뀌었어요. 다시 열어 확인해 주세요".into(),
        "reconnect_required" => "연결이 풀렸어요. 다시 연결해 주세요".into(),
        "feature_missing" => "이 연결에 그 권한이 없어요. 다시 연결하며 권한을 허용해 주세요".into(),
        "repo_not_accessible" => "그 레포에 GitHub 앱이 설치되지 않았어요. 「앱 설치」로 레포를 고른 뒤 다시 승인해 주세요".into(),
        "provider_rejected" => format!(
            "받는 쪽이 거절했어요 — {}",
            error.split_once(": ").map_or("", |(_, detail)| detail)
        ),
        "result_unknown" => "보냈지만 결과를 못 받았어요. GitHub 에서 확인해 주세요".into(),
        "setup_required" => "관문에 이 연결이 아직 준비되지 않았어요".into(),
        "update_required" => "관문이 이 기능을 아직 몰라요. 관문 업데이트가 필요해요".into(),
        "cancelled" => "연결을 취소했어요".into(),
        "not_found" => "이미 처리됐거나 사라진 항목이에요".into(),
        "gateway_unreachable" | "oauth_unavailable" => "관문에 닿지 못했어요. 연결 상태를 확인해 주세요".into(),
        _ => "요청을 마치지 못했어요. 다시 시도해 주세요".into(),
    }
}

impl State {
    fn pending(&self) -> &[serde_json::Value] {
        self.data["pending"].as_array().map_or(&[], Vec::as_slice)
    }

    pub(crate) fn view(&self, signed_in: bool) -> View {
        View {
            signed_in: signed_in || self.fixture,
            data: self.data.clone(),
            open: self.open.as_ref().map(|(id, _)| id.clone()),
            disconnect: self.disconnect.clone(),
            busy: self.busy.is_some(),
            message: self.message.clone(),
        }
    }

    /// Fetches the list in the background once a minute while signed in — also when settings are
    /// closed, so a new pending write can raise a notification.
    fn refresh_due(&mut self, signed_in: bool, force: bool) {
        if !signed_in || self.fetch.is_some() || self.fixture {
            return;
        }
        if !force && self.fetched.is_some_and(|at| at.elapsed() < REFRESH) {
            return;
        }
        self.fetched = Some(std::time::Instant::now());
        let (tx, rx) = mpsc::channel();
        self.fetch = Some(rx);
        std::thread::spawn(move || {
            let result = kasa_mcp::device_auth::handle(&serde_json::json!({"op":"connections"}))
                .map_err(|error| error.to_string());
            let _ = tx.send(Done::Listed(result));
        });
    }

    /// New pending writes since the last look, as (id, one-line summary).
    fn fresh_pending(&mut self) -> Vec<(String, String)> {
        let mut fresh = Vec::new();
        for pending in self.pending() {
            let Some(id) = pending["id"].as_str() else { continue };
            if !self.notified.contains(id) {
                fresh.push((id.to_string(), summary(pending)));
            }
        }
        for (id, _) in &fresh {
            self.notified.insert(id.clone());
        }
        fresh
    }

    /// Returns true when the page should repaint.
    fn poll(&mut self) -> (bool, Vec<(String, String)>) {
        let mut changed = false;
        let mut fresh = Vec::new();
        let first = self.data.is_null();
        if let Some(Ok(Done::Listed(result))) = self.fetch.as_ref().map(Receiver::try_recv) {
            self.fetch = None;
            changed = true;
            match result {
                Ok(data) => {
                    self.data = data;
                    let ids: Vec<&str> =
                        self.pending().iter().filter_map(|p| p["id"].as_str()).collect();
                    if self.open.as_ref().is_some_and(|(id, _)| !ids.contains(&id.as_str())) {
                        self.open = None;
                    }
                    let new = self.fresh_pending();
                    // Writes already waiting when the app started were not «new» to the person.
                    if !first {
                        fresh = new;
                    }
                }
                Err(error) if error.contains("signed_out") || error.contains("isolated_run") => {
                    self.data = serde_json::Value::Null;
                }
                Err(_) => {}
            }
        }
        if let Some(Ok(Done::Acted(result))) = self.busy.as_ref().map(Receiver::try_recv) {
            self.busy = None;
            changed = true;
            match result {
                Ok(message) => {
                    self.open = None;
                    self.disconnect = None;
                    self.message = Some((message, false));
                }
                Err(error) => self.message = Some((readable(&error), true)),
            }
            self.fetched = None;
        }
        (changed, fresh)
    }

    /// Verification-run screens: two connections and two pending PRs, the first opened when `open`.
    pub(crate) fn fixture(&mut self, open: bool) {
        self.fixture = true;
        self.data = serde_json::json!({
            "available":{"google":false,"github":true},
            "github_install_url":"https://github.com/apps/kasa-work/installations/new",
            "connections":[
                {"id":"con_a","provider":"github","display":"octo","features":["github.pr"],"state":"ok"},
                {"id":"con_b","provider":"github","display":"octo-work","features":["github.pr"],"state":"reconnect_required"}],
            "pending":[
                {"id":"pw_a","digest":"d","provider":"github","display":"octo","device_label":"건호의 MacBook Pro · 케이",
                 "write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/login-first","title":"로그인 첫 화면 정리",
                 "body":"이번 판에서 한 일\n- 로그인 첫 화면 정리\n- PR 일 권한\n\n폰 화면은 다음 판에 붙입니다.","draft":false}},
                {"id":"pw_b","digest":"d","provider":"github","display":"octo","device_label":"맥미니 · 나쵸",
                 "write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/work","title":"일 권한 화면","body":"","draft":true}}]});
        self.open = open.then(|| ("pw_a".to_string(), "d".to_string()));
    }

    pub(crate) fn hide(&mut self) {
        self.disconnect = None;
    }

    /// A new connection was just made from the account page; show it without waiting a minute.
    pub(crate) fn refresh_now(&mut self) {
        self.refresh_due(true, true);
    }

    fn run(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        self.message = None;
        std::thread::spawn(move || {
            let _ = tx.send(Done::Acted(work()));
        });
    }
}

fn summary(pending: &serde_json::Value) -> String {
    let write = &pending["write"];
    match write["kind"].as_str() {
        Some("pr") => format!(
            "PR · {} {} → {} · {}",
            write["repo"].as_str().unwrap_or(""),
            write["head"].as_str().unwrap_or(""),
            write["base"].as_str().unwrap_or(""),
            write["title"].as_str().unwrap_or("")
        ),
        _ => "알 수 없는 쓰기".into(),
    }
}

/// The whole write as the person approves it: repository, branches, the full title and body.
fn detail(pending: &serde_json::Value) -> Vec<(String, String)> {
    let write = &pending["write"];
    let mut rows = vec![(
        "보낼 계정".to_string(),
        format!("GitHub · {}", pending["display"].as_str().unwrap_or("")),
    )];
    match write["kind"].as_str() {
        Some("pr") => {
            rows.push(("레포".into(), write["repo"].as_str().unwrap_or("").into()));
            rows.push((
                "브랜치".into(),
                format!(
                    "{} → {}{}",
                    write["head"].as_str().unwrap_or(""),
                    write["base"].as_str().unwrap_or(""),
                    if write["draft"] == true { " (초안)" } else { "" }
                ),
            ));
            rows.push(("제목".into(), write["title"].as_str().unwrap_or("").into()));
            rows.push(("본문".into(), write["body"].as_str().unwrap_or("").into()));
        }
        _ => {}
    }
    rows.push((
        "요청한 곳".into(),
        pending["device_label"].as_str().unwrap_or("").into(),
    ));
    rows
}

impl App {
    /// Background list refresh, notifications for new pending writes, and finished actions.
    pub(crate) fn work_permissions_poll(&mut self) {
        let signed_in = self.device_account.status["logged_in"] == true;
        let work = &mut self.device_account.work;
        work.refresh_due(signed_in, false);
        let (changed, fresh) = work.poll();
        for (id, line) in fresh {
            crate::chrome::notify_desktop(
                "승인을 기다리는 일",
                &line,
                None,
                Some(&format!("work-pending:{id}")),
                None,
            );
        }
        if changed {
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn work_permissions_action(&mut self, act: Act) {
        let work = &mut self.device_account.work;
        if work.busy.is_some() {
            return;
        }
        work.message = None;
        match act {
            Act::InstallGithub => {
                if let Some(url) = work.data["github_install_url"].as_str() {
                    crate::chrome::open_url_in_browser(url);
                }
            }
            Act::AskDisconnect(index) => {
                work.disconnect = work.data["connections"][index]["id"].as_str().map(str::to_string);
            }
            Act::KeepConnection => work.disconnect = None,
            Act::Disconnect => {
                let Some(id) = work.disconnect.clone() else { return };
                work.run(move || {
                    let value = kasa_mcp::device_auth::handle(&serde_json::json!({"op":"disconnect","id":id}))
                        .map_err(|error| error.to_string())?;
                    Ok(if value["provider_revoked"] == true {
                        "연결을 끊고 권한도 돌려줬어요".into()
                    } else {
                        "연결을 끊었어요. 깃허브 보안 설정에서도 앱 권한을 지울 수 있어요".into()
                    })
                });
            }
            Act::Open(index) => {
                work.open = work.pending().get(index).and_then(|pending| {
                    Some((pending["id"].as_str()?.to_string(), pending["digest"].as_str()?.to_string()))
                });
            }
            Act::Close => work.open = None,
            Act::Approve => {
                let Some((id, digest)) = work.open.clone() else { return };
                work.run(move || {
                    let value = kasa_mcp::device_auth::connections::approve(&id, &digest)
                        .map_err(|error| error.to_string())?;
                    Ok(match value["status"].as_str() {
                        Some("created") => format!("PR #{} 을 만들었어요", value["number"]),
                        _ => "처리했어요".to_string(),
                    })
                });
            }
            Act::Reject => {
                let Some((id, _)) = work.open.clone() else { return };
                work.run(move || {
                    kasa_mcp::device_auth::handle(&serde_json::json!({"op":"reject","id":id}))
                        .map(|_| "요청을 버렸어요".to_string())
                        .map_err(|error| error.to_string())
                });
            }
        }
        self.chrome_dirty = true;
    }
}

fn act(a: Act) -> Target {
    Target::Setting(SettingsAction::DeviceAccount(Action::Work(a)))
}

fn feature_label(features: &serde_json::Value) -> &'static str {
    if features.as_array().is_some_and(|features| features.iter().any(|f| f == "github.pr")) {
        "PR 요청"
    } else {
        "권한 없음"
    }
}

pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let v = &s.device_account.work;
    g.queue_icon("plug", x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, "일 권한", 14.5, theme::text(), true);
    *y += 26.0;
    if !v.signed_in {
        info_slab(
            g,
            x,
            y,
            w,
            "KASA 계정에 로그인하면 GitHub 을 연결해 학생·나쵸가 PR 을 요청하게 할 수 있어요. PR 은 늘 여기서 승인해야 열려요.",
        );
        *y += 14.0;
        return;
    }
    let connections = v.data["connections"].as_array().cloned().unwrap_or_default();
    for (index, connection) in connections.iter().enumerate() {
        let broken = connection["state"] == "reconnect_required";
        let confirming = v.disconnect.as_deref() == connection["id"].as_str();
        let controls_w = if confirming { 150.0 } else { 64.0 };
        g.queue_icon("github", x, *y + 11.0, 14.0, theme::text());
        let label = fit(
            g,
            &format!("GitHub · {}", connection["display"].as_str().unwrap_or("")),
            w - controls_w - 28.0,
            12.0,
            false,
        );
        draw_text(g, x + 22.0, *y + 4.0, &label, 12.0, theme::text(), false);
        let sub = if broken { "다시 연결 필요" } else { feature_label(&connection["features"]) };
        draw_text(
            g,
            x + 22.0,
            *y + 21.0,
            sub,
            10.5,
            if broken { theme::danger() } else { theme::text_dim() },
            false,
        );
        let cy = *y + (ROW_H - CTL_H) / 2.0;
        if confirming {
            button(g, s, hits, (x + w - 150.0, cy, 82.0, CTL_H), "정말 끊기", act(Act::Disconnect), false);
            button(g, s, hits, (x + w - 60.0, cy, 60.0, CTL_H), "취소", act(Act::KeepConnection), false);
        } else if !v.busy {
            button(g, s, hits, (x + w - 64.0, cy, 64.0, CTL_H), "끊기", act(Act::AskDisconnect(index)), false);
        }
        *y += ROW_H;
    }
    if v.data.is_null() {
        info_slab(g, x, y, w, "연결 목록을 받는 중…");
    } else if v.data["available"]["github"] != true {
        info_slab(g, x, y, w, "관문에 GitHub 연결이 아직 준비되지 않았어요.");
    } else {
        // 따로 붙이는 단추는 없다 — 위 「로그인 방법」의 연결 한 번이 로그인과 일 권한을 함께 붙인다.
        info_slab(
            g,
            x,
            y,
            w,
            if connections.is_empty() {
                "위 「로그인 방법」에서 GitHub 을 연결하면 로그인과 함께 PR 권한이 붙어요."
            } else if connections.iter().any(|c| c["state"] == "reconnect_required") {
                "「다시 연결 필요」는 위 「로그인 방법」의 그 줄에서 「다시 연결」을 누르면 돼요."
            } else {
                "학생은 kasaterm-cli pr, 나쵸는 kasa-device work 로 써요. PR 만들기는 아래에서 승인해야 열려요."
            },
        );
        if connections.iter().any(|c| c["provider"] == "github") && v.data["github_install_url"].is_string() {
            button(g, s, hits, (x, *y, 150.0_f32.min(w), CTL_H), "PR 올릴 레포 고르기", act(Act::InstallGithub), false);
            *y += ROW_H;
        }
    }
    let pending = v.data["pending"].as_array().cloned().unwrap_or_default();
    if !pending.is_empty() {
        draw_text(g, x, *y + 6.0, "승인을 기다리는 일", 11.0, theme::text_dim(), false);
        *y += 28.0;
    }
    for (index, item) in pending.iter().enumerate() {
        let open = v.open.as_deref() == item["id"].as_str();
        let line = fit(g, &summary(item), w - 72.0, 12.0, open);
        draw_text(g, x, *y + 4.0, &line, 12.0, theme::text(), open);
        let from = fit(
            g,
            &format!("요청: {}", item["device_label"].as_str().unwrap_or("")),
            w - 72.0,
            10.5,
            false,
        );
        draw_text(g, x, *y + 21.0, &from, 10.5, theme::text_dim(), false);
        if !open && !v.busy {
            button(g, s, hits, (x + w - 64.0, *y + (ROW_H - CTL_H) / 2.0, 64.0, CTL_H), "보기", act(Act::Open(index)), false);
        }
        *y += ROW_H;
        if !open {
            continue;
        }
        let label_w = 72.0;
        for (name, value) in detail(item) {
            draw_text(g, x, *y, &name, 10.5, theme::text_dim(), false);
            let lines: Vec<String> = value
                .lines()
                .flat_map(|line| {
                    let wrapped = wrap_words(g, line, w - label_w, 12.0);
                    if wrapped.is_empty() { vec![String::new()] } else { wrapped }
                })
                .collect();
            let shown = lines.len().min(MAX_SHOWN_LINES);
            for line in &lines[..shown] {
                draw_text(g, x + label_w, *y, line, 12.0, theme::text(), false);
                *y += 18.0;
            }
            if lines.len() > shown {
                let more = format!("… {}줄 더 — 끝까지 보고 승인하려면 요청한 쪽에서 내용을 줄여 주세요", lines.len() - shown);
                let more = fit(g, &more, w - label_w, 10.5, false);
                draw_text(g, x + label_w, *y, &more, 10.5, theme::text_dim(), false);
                *y += 18.0;
            }
            if lines.is_empty() {
                *y += 18.0;
            }
            *y += 4.0;
        }
        let complete = detail(item)
            .iter()
            .all(|(_, value)| value.lines().count() <= MAX_SHOWN_LINES);
        let go = "PR 만들기";
        if v.busy {
            draw_text(g, x, *y + 4.0, "처리 중…", 12.0, theme::text_dim(), false);
            *y += ROW_H;
        } else {
            let mut bx = x;
            for (label, a, primary) in [(go, Act::Approve, true), ("버리기", Act::Reject, false), ("접기", Act::Close, false)] {
                if a == Act::Approve && !complete {
                    continue;
                }
                let bw = g.measure_chrome_text(label, 12.0, primary) + 32.0;
                button(g, s, hits, (bx, *y, bw, CTL_H), label, act(a), primary);
                bx += bw + 8.0;
            }
            *y += ROW_H;
        }
    }
    if let Some((message, error)) = &v.message {
        for line in wrap_words(g, message, w, 10.5) {
            draw_text(g, x, *y, &line, 10.5, if *error { theme::danger() } else { theme::text_dim() }, false);
            *y += 18.0;
        }
    }
    *y += 24.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str) -> serde_json::Value {
        serde_json::json!({"id":id,"digest":format!("d-{id}"),"provider":"github","display":"octo",
            "device_label":"Laptop","write":{"kind":"pr","repo":"2rami/kasaterm","base":"main","head":"feat/x",
            "title":"주간 정리","body":"한 줄\n두 줄","draft":true}})
    }

    #[test]
    fn approval_acts_on_the_opened_write_even_after_the_list_moves() {
        let mut state = State {
            data: serde_json::json!({"pending":[pending("pw_a"), pending("pw_b")]}),
            ..State::default()
        };
        state.open = state.pending().get(1).map(|p| (p["id"].as_str().unwrap().into(), p["digest"].as_str().unwrap().into()));
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Listed(Ok(serde_json::json!({"pending":[pending("pw_b")]})))).unwrap();
        state.poll();
        assert_eq!(state.open, Some(("pw_b".into(), "d-pw_b".into())), "the opened write changed under the person");
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Listed(Ok(serde_json::json!({"pending":[]})))).unwrap();
        state.poll();
        assert!(state.open.is_none(), "a handled write stayed open for approval");
    }

    #[test]
    fn only_writes_that_arrive_later_notify() {
        let mut state = State::default();
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Listed(Ok(serde_json::json!({"pending":[pending("pw_old")]})))).unwrap();
        assert!(state.poll().1.is_empty(), "a write from before the app started notified");
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Listed(Ok(serde_json::json!({"pending":[pending("pw_old"), pending("pw_new")]})))).unwrap();
        let fresh = state.poll().1;
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].0, "pw_new");
        assert_eq!(fresh[0].1, "PR · 2rami/kasaterm feat/x → main · 주간 정리");
    }

    #[test]
    fn the_approval_view_shows_the_branches_and_the_whole_body() {
        let rows = detail(&pending("pw_a"));
        assert!(rows.contains(&("브랜치".into(), "feat/x → main (초안)".into())));
        assert!(rows.contains(&("본문".into(), "한 줄\n두 줄".into())));
    }
}
