//! 설정 「계정」의 「로그인 동기화」 — 카사 키체인의 웹 로그인을 같은 계정의 기기끼리 맞춘다(docs/login-vault-sync.md).
//! 첫 기기는 「금고 만들기」, 다음 기기는 「이 기기 등록」(폰 Face ID 승인 뒤 열쇠를 가진 기기가 열쇠를 넘긴다).
//! 화면에는 개수와 지문만 보인다 — 사이트·아이디는 어디에도 그리지 않는다.

use super::*;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Create,
    Join,
    /// 두 기기 화면의 열쇠 지문이 같다.
    ConfirmFingerprint,
    /// 한 번에 많이 사라진 로그인을 정말 지운다.
    AllowDeletions,
    /// 그 폰 열쇠를 금고 승인에 믿기(지문 확인 창을 연다).
    ConfirmPhone(usize),
    /// 북마크·탭 동기화 켜고 끄기(kasa-device browser_sync). 금고 열쇠의 갈래로 잠근다.
    Bookmarks(bool),
    Tabs(bool),
    /// 한 번에 많이 사라진 북마크를 정말 지운다.
    AllowBookmarkDeletions,
}

/// 확인 창의 답 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static PHONE_ANSWER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Default)]
pub(crate) struct State {
    status: serde_json::Value,
    browser: serde_json::Value,
    busy: Option<Receiver<Result<String, String>>>,
    message: Option<(String, bool)>,
    fixture: bool,
}

#[derive(Clone, Default)]
pub(crate) struct View {
    signed_in: bool,
    status: serde_json::Value,
    browser: serde_json::Value,
    busy: bool,
    message: Option<(String, bool)>,
    /// 금고 승인에 아직 직접 믿지 않은 폰 — 직접 믿은 폰이 하나도 없을 때만 찬다.
    phones: Vec<serde_json::Value>,
}

impl State {
    pub(crate) fn view(&self, signed_in: bool) -> View {
        View {
            signed_in: signed_in || self.fixture,
            status: self.status.clone(),
            browser: self.browser.clone(),
            busy: self.busy.is_some(),
            message: self.message.clone(),
            phones: self.unconfirmed_phones(),
        }
    }

    /// 동기화 스레드가 쓴 상태를 옮겨 온다 — 메모리 읽기라 그리기마다 불러도 된다.
    fn poll(&mut self) -> bool {
        let mut changed = false;
        if !self.fixture {
            let status = kasa_device::login_vault::status();
            if status != self.status {
                self.status = status;
                changed = true;
            }
            let browser = kasa_device::browser_sync::status();
            if browser != self.browser {
                self.browser = browser;
                changed = true;
            }
        }
        match self.busy.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => {
                self.busy = None;
                self.message = Some(match result {
                    Ok(message) => (message, false),
                    Err(error) => (error, true),
                });
                changed = true;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.busy = None;
                changed = true;
            }
            _ => {}
        }
        changed
    }

    fn run(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        self.message = None;
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
    }

    /// 사람이 직접 믿은 폰이 없고, 자동으로 믿은 폰만 있다.
    fn unconfirmed_phones(&self) -> Vec<serde_json::Value> {
        let phones = self.status["phones"].as_array().cloned().unwrap_or_default();
        if phones.iter().any(|p| p["manual"] == true) {
            return Vec::new();
        }
        phones
    }

    /// 검증 실행 화면 — 동기화 스레드 상태 대신 정해 둔 상태를 그린다.
    pub(crate) fn fixture(&mut self, state: &str) {
        self.fixture = true;
        let phones = serde_json::json!([{"id": "a1", "label": "내 iPhone", "fingerprint": "3F2A-91C0-7D4E-55B1", "manual": false}]);
        self.browser = serde_json::json!({"state": "synced", "settings": {"bookmarks": true, "tabs": false}, "bookmarks": 128, "devices": 1});
        self.status = match state {
            "joining" => serde_json::json!({"state": "joining", "step": "phone", "code": "7F3A-91C2-0B5D-E44A"}),
            "holder" => serde_json::json!({"state": "joining", "step": "holder", "code": "7F3A-91C2-0B5D-E44A"}),
            "synced" => serde_json::json!({"state": "synced", "fingerprint": "K7Q2-MX9D-4HTW-1PZE", "items": 20, "held": 1, "phones": phones}),
            "clock" => serde_json::json!({"state": "synced", "fingerprint": "K7Q2-MX9D-4HTW-1PZE", "items": 20, "clock_refused": true, "phones": phones}),
            "confirm" => {
                let hour_ago = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64).saturating_sub(3_600_000);
                serde_json::json!({"state": "confirm_fingerprint", "fingerprint": "K7Q2-MX9D-4HTW-1PZE", "created_by": "내 MacBook Pro", "created_ms": hour_ago})
            }
            "deletions" => serde_json::json!({"state": "deletions_pending", "fingerprint": "K7Q2-MX9D-4HTW-1PZE", "items": 20, "deletions": 20}),
            "mismatch" => serde_json::json!({"state": "key_mismatch", "message": "이 기기의 금고 열쇠가 계정 금고와 달라요. 다시 등록해야 해요", "fingerprint": "K7Q2-MX9D-4HTW-1PZE"}),
            other => serde_json::json!({"state": other}),
        };
    }
}

impl App {
    pub(crate) fn login_vault_poll(&mut self) {
        if let Some(id) = PHONE_ANSWER.lock().ok().and_then(|mut a| a.take()) {
            self.device_account.vault.run(move || {
                kasa_device::login_vault::confirm_phone(&id).map(|()| "이 폰으로 새 기기 등록을 허락할 수 있어요".to_string())
            });
        }
        if self.device_account.vault.poll() {
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
    }

    pub(crate) fn login_vault_action(&mut self, act: Act) {
        let vault = &mut self.device_account.vault;
        if vault.busy.is_some() || vault.fixture {
            return;
        }
        match act {
            Act::Create => vault.run(|| {
                kasa_device::login_vault::create_vault().map(|fingerprint| format!("금고를 만들었어요 · 열쇠 지문 {fingerprint}"))
            }),
            Act::Join => {
                let label = crate::render::pane_identity::local_device_name().unwrap_or_else(|| "새 기기".into());
                vault.run(move || kasa_device::login_vault::start_join(&label).map(|()| String::new()));
            }
            Act::ConfirmFingerprint => vault.run(|| kasa_device::login_vault::confirm_fingerprint().map(|()| "맞추기 시작해요".to_string())),
            Act::AllowDeletions => {
                kasa_device::login_vault::allow_deletions();
                vault.message = Some(("다음 맞추기에서 지워요".into(), false));
            }
            Act::Bookmarks(on) => vault.run(move || {
                kasa_device::browser_sync::set(Some(on), None).map(|_| if on { "북마크를 맞추기 시작해요" } else { "북마크 맞추기를 껐어요" }.to_string())
            }),
            Act::Tabs(on) => vault.run(move || {
                kasa_device::browser_sync::set(None, Some(on)).map(|_| if on { "열린 탭을 보내기 시작해요" } else { "탭 보내기를 껐어요 · 다른 기기에서도 사라져요" }.to_string())
            }),
            Act::AllowBookmarkDeletions => {
                kasa_device::browser_sync::allow_deletions();
                vault.message = Some(("다음 맞추기에서 지워요".into(), false));
            }
            Act::ConfirmPhone(index) => {
                let Some(phone) = vault.unconfirmed_phones().get(index).cloned() else { return };
                let info = format!(
                    "{}\n지문  {}\n\n폰 설정 → 계정 → Face ID 승인 열쇠의 지문 16자가 모두 같은지 맞춰 보세요. 자동으로 믿은 열쇠라 관문이 끼워 넣었을 수도 있어요. 믿으면 이 폰의 Face ID 로 새 기기에 로그인 금고 열쇠를 넘기는 것을 허락할 수 있어요.",
                    phone["label"].as_str().unwrap_or("폰"),
                    phone["fingerprint"].as_str().unwrap_or("")
                );
                let id = phone["id"].as_str().unwrap_or_default().to_string();
                if let Some(window) = &self.window {
                    let _ = super::op::confirm_sheet(window, "이 폰을 금고 승인에 믿을까요?", &info, "믿기", &PHONE_ANSWER, id);
                }
            }
        }
        self.chrome_dirty = true;
    }
}

fn act(a: Act) -> Target {
    Target::Setting(SettingsAction::DeviceAccount(Action::Vault(a)))
}

/// 상태 한 줄과 그 아래 흐린 설명, 오른쪽 단추 하나.
#[allow(clippy::too_many_arguments)]
fn row(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32, line: &str, dim: &str, error: bool, button_: Option<(&str, Act)>) {
    // 단추 폭은 그리는 쪽(공통 버튼)과 같은 잣대로 잰다 — 글자 폭으로 따로 재면 굵은 글씨가 잘린다.
    let width = |g: &mut gpu::GpuRenderer, label: &str| crate::native_controls::button_width(g, label, CTL_H, true);
    let reserve = button_.map_or(0.0, |(label, _)| width(g, label) + 12.0);
    let line = fit(g, line, w - reserve, 12.0, false);
    draw_text(g, x, *y + 4.0, &line, 12.0, if error { theme::danger() } else { theme::text() }, false);
    let dim = fit(g, dim, w - reserve, 10.5, false);
    draw_text(g, x, *y + 21.0, &dim, 10.5, theme::text_dim(), false);
    if let Some((label, a)) = button_ {
        let bw = width(g, label);
        button(g, s, hits, (x + w - bw, *y + (ROW_H - CTL_H) / 2.0, bw, CTL_H), label, act(a), true);
    }
    *y += ROW_H;
}

fn note(g: &mut gpu::GpuRenderer, x: f32, y: &mut f32, w: f32, text: &str) {
    for line in wrap_words(g, text, w, 10.5) {
        draw_text(g, x, *y, &line, 10.5, theme::text_dim(), false);
        *y += 18.0;
    }
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    let v = &s.device_account.vault;
    g.queue_icon("lock", x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, "로그인 동기화", 14.5, theme::text(), true);
    *y += 26.0;
    if !v.signed_in {
        info_slab(g, x, y, w, "KASA 계정에 로그인하면 카사 키체인의 웹 로그인을 이 계정의 다른 기기와 맞출 수 있어요.");
        *y += 14.0;
        return;
    }
    let st = &v.status;
    let message = st["message"].as_str().unwrap_or_default();
    let fingerprint = st["fingerprint"].as_str().unwrap_or_default();
    let idle = !v.busy;
    match st["state"].as_str().unwrap_or("checking") {
        "no_vault" => row(g, s, hits, x, y, w, "아직 계정에 금고가 없어요", "이 기기의 웹 로그인을 잠가서 계정에 올리는 첫 기기가 돼요", false, idle.then_some(("금고 만들기", Act::Create))),
        "needs_join" => row(g, s, hits, x, y, w, "계정에 금고가 있어요", "폰 Face ID 로 이 기기를 등록하면 열쇠를 가진 기기가 열쇠를 넘겨줘요", false, idle.then_some(("이 기기 등록", Act::Join))),
        "joining" if st["step"] == "holder" => row(g, s, hits, x, y, w, "열쇠를 가진 기기를 기다리는 중…", "그 기기가 켜져 있어야 해요 · 최대 10분", false, None),
        "joining" => {
            let code = st["code"].as_str().unwrap_or_default();
            row(g, s, hits, x, y, w, "폰에서 Face ID 로 허락해 주세요", &format!("기기 지문 {code} — 폰에 보이는 지문과 같은지 보세요"), false, None)
        }
        "confirm_fingerprint" => {
            let by = st["created_by"].as_str().filter(|b| !b.is_empty()).unwrap_or("이름 없는 기기");
            let when = st["created_ms"].as_u64().filter(|ms| *ms > 0).map(crate::machinescol::clock_label).unwrap_or_default();
            row(g, s, hits, x, y, w, &format!("열쇠 지문 {fingerprint}"), &format!("금고를 만든 기기 {by} · {when}"), false, idle.then_some(("지문이 같아요", Act::ConfirmFingerprint)));
            note(g, x, y, w, "내 다른 기기의 설정에서 이 지문을 본 적이 없으면 누르지 마세요. 금고를 만든 기기가 모르는 기기여도 누르지 마세요 — 누르기 전에는 이 기기의 로그인을 받지도 올리지도 않아요.");
        }
        "deletions_pending" => {
            let n = st["deletions"].as_u64().unwrap_or(0);
            row(g, s, hits, x, y, w, &format!("지우기 {n}개 보류 중"), "이 기기에서 로그인이 한꺼번에 사라졌어요 — 정말 지운 것인지 확인해 주세요", true, idle.then_some(("지우기 확인", Act::AllowDeletions)));
        }
        "synced" | "joined" => {
            let items = st["items"].as_u64().unwrap_or(0);
            let line = if st["state"] == "joined" { "등록했어요 · 지문을 확인하면 맞추기 시작해요".to_string() } else { format!("웹 로그인 {items}개를 맞추고 있어요") };
            row(g, s, hits, x, y, w, &line, &format!("열쇠 지문 {fingerprint}"), false, None);
            if st["clock_refused"] == true {
                note(g, x, y, w, "새 기기 등록을 시계 때문에 거절했어요 — 이 기기와 새 기기의 시계를 맞춘 뒤 새 기기에서 다시 등록해 주세요.");
            }
            let held = st["held"].as_u64().unwrap_or(0);
            if held > 0 {
                note(g, x, y, w, &format!("다른 기기에서 지운 로그인 {held}개를 이 기기 키체인이 못 지웠어요 — 브라우저에서 지워 주세요"));
            }
        }
        "clock_mismatch" => row(g, s, hits, x, y, w, message, "기기 시계를 맞추면 다음 바퀴에 다시 맞춰요", true, None),
        "key_mismatch" | "join_failed" => row(g, s, hits, x, y, w, message, "열쇠는 폰 Face ID 승인으로만 다시 받아요", true, idle.then_some(("이 기기 등록", Act::Join))),
        "unsupported_platform" => row(g, s, hits, x, y, w, "이 OS 에서는 아직 맞추지 않아요", "맥과 Windows 에서 써요", false, None),
        "signed_out" | "checking" => row(g, s, hits, x, y, w, "확인하는 중…", "", false, None),
        _ => {
            let line = if message.is_empty() { "동기화하지 못했어요" } else { message };
            row(g, s, hits, x, y, w, line, "로컬 키체인의 로그인은 그대로예요", true, None)
        }
    }
    // 새 기기에 열쇠를 넘기는 승인은 사람이 이 화면에서 직접 믿은 폰만 받는다 — 자동으로 믿은 폰뿐이면 여기서 믿게 한다.
    if !v.phones.is_empty() {
        for (index, phone) in v.phones.iter().enumerate() {
            let label = format!("{} · {}", phone["label"].as_str().unwrap_or("폰"), phone["fingerprint"].as_str().unwrap_or(""));
            row(g, s, hits, x, y, w, &label, "이 폰으로 새 기기 등록을 허락하려면 지문을 맞춰 보고 믿어 주세요", false, idle.then_some(("금고 승인에 믿기…", Act::ConfirmPhone(index))));
        }
    }
    info_slab(
        g,
        x,
        y,
        w,
        "비밀번호는 이 기기에서 잠근 뒤에만 계정에 올라가요. 관문은 사이트·아이디·비밀번호를 볼 수 없고, 새 기기는 이 화면에서 믿은 폰의 Face ID 승인으로만 열쇠를 받아요.",
    );
    if let Some((message, error)) = v.message.as_ref().filter(|(m, _)| !m.is_empty()) {
        for line in wrap_words(g, message, w, 10.5) {
            draw_text(g, x, *y, &line, 10.5, if *error { theme::danger() } else { theme::text_dim() }, false);
            *y += 18.0;
        }
    }
    *y += 24.0;
    paint_browser(g, s, hits, x, y, w);
}

/// 「북마크·탭 동기화」 — 카사브라우저의 북마크를 맞추고 열린 탭을 보낸다. 금고 열쇠가 있고 지문을 확인한 기기에서만.
fn paint_browser(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    let v = &s.device_account.vault;
    if !v.signed_in {
        return;
    }
    g.queue_icon("lock", x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, "북마크·탭 동기화", 14.5, theme::text(), true);
    *y += 26.0;
    let b = &v.browser;
    let ready = matches!(v.status["state"].as_str(), Some("synced"));
    if !ready || b["state"] == "needs_vault" {
        info_slab(g, x, y, w, "위의 로그인 동기화를 켜고 지문을 확인한 기기에서 쓸 수 있어요. 같은 금고 열쇠에서 가른 열쇠로 잠가서 따로 등록하지 않아요.");
        *y += 24.0;
        return;
    }
    let bookmarks_on = b["settings"]["bookmarks"] == true;
    let tabs_on = b["settings"]["tabs"] == true;
    let idle = !v.busy;
    let state = b["state"].as_str().unwrap_or("checking");
    let count = b["bookmarks"].as_u64().unwrap_or(0);
    let marks_dim = match state {
        _ if !bookmarks_on => "같은 계정 프로필의 북마크를 기기끼리 합쳐요".to_string(),
        "browser_closed" => "카사브라우저가 켜지면 맞춰요".to_string(),
        "synced" | "deletions_pending" => format!("북마크 {count}개를 맞추고 있어요"),
        _ => "확인하는 중…".to_string(),
    };
    let toggle = |on: bool| if on { "끄기" } else { "켜기" };
    row(g, s, hits, x, y, w, "북마크 맞추기", &marks_dim, false, idle.then_some((toggle(bookmarks_on), Act::Bookmarks(!bookmarks_on))));
    let devices = b["devices"].as_u64().unwrap_or(0);
    let tabs_dim = if tabs_on { format!("다른 기기 {devices}대의 탭을 방문 기록 패널에서 봐요") } else { "열린 탭을 보내고 다른 기기의 탭을 받아요".to_string() };
    row(g, s, hits, x, y, w, "탭 보내기·받기", &tabs_dim, false, idle.then_some((toggle(tabs_on), Act::Tabs(!tabs_on))));
    let message = b["message"].as_str().unwrap_or_default();
    match state {
        "deletions_pending" => {
            let n = b["deletions"].as_u64().unwrap_or(0);
            row(g, s, hits, x, y, w, &format!("북마크 지우기 {n}개 보류 중"), "이 기기에서 북마크가 한꺼번에 사라졌어요 — 정말 지운 것인지 확인해 주세요", true, idle.then_some(("지우기 확인", Act::AllowBookmarkDeletions)));
        }
        _ if !message.is_empty() => row(g, s, hits, x, y, w, message, "이 기기의 북마크와 탭은 그대로예요", true, None),
        _ => {}
    }
    info_slab(g, x, y, w, "북마크와 탭은 이 기기에서 잠근 뒤에만 계정에 올라가요. 관문은 주소·제목을 볼 수 없고, 에이전트 작업공간과 에이전트가 연 탭은 보내지 않아요.");
    *y += 24.0;
}
