//! 설정 「계정」의 「1Password」 — 학생이 폰 Face ID 한 번으로 비밀을 읽게 하는 맥 쪽 설정
//! (docs/op-faceid-approval.md). 토큰 넣기·지우기와 폰 열쇠 믿기는 이 화면에서만 난다 — 소켓·CLI 에는 없다.
//! 토큰은 암호 칸(NSSecureTextField)에서 곧장 kasa-mcp 로 가 키체인에 들어가고, 이 화면 상태에는 남지 않는다.

use super::*;
use std::collections::HashSet;
use std::sync::Mutex;

const REFRESH: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    AskToken,
    AskClear,
    Clear,
    KeepToken,
    Trust(usize),
    Untrust(usize),
}

#[derive(Debug)]
enum Done {
    Loaded(serde_json::Value, Result<Vec<serde_json::Value>, String>),
    Acted(Result<String, String>),
}

/// 시트의 답 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static TOKEN_ANSWER: Mutex<Option<String>> = Mutex::new(None);
static TRUST_ANSWER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Default)]
pub(crate) struct State {
    status: serde_json::Value,
    untrusted: Vec<serde_json::Value>,
    fetched: Option<std::time::Instant>,
    fetch: Option<Receiver<Done>>,
    busy: Option<Receiver<Done>>,
    confirm_clear: bool,
    message: Option<(String, bool)>,
    notified: HashSet<String>,
    loaded_once: bool,
    fixture: bool,
    /// 검증 실행 전용 — 띄운 믿기 시트와 그 단추를 누를 시각(`KASATERM_OP_AUTOTRUST_MS`).
    auto: Option<(usize, std::time::Instant)>,
}

#[derive(Clone, Default)]
pub(crate) struct View {
    signed_in: bool,
    status: serde_json::Value,
    untrusted: Vec<serde_json::Value>,
    confirm_clear: bool,
    busy: bool,
    message: Option<(String, bool)>,
}

fn readable(error: &str) -> String {
    let code = error.split(':').next().unwrap_or(error).trim();
    match code {
        "token_invalid" => "서비스 계정 토큰 모양이 아니에요(ops_ 로 시작해요)".into(),
        "vault_count" => format!(
            "이 토큰은 금고 {}개를 볼 수 있어요. 학생용 전용 금고 하나만 읽기로 연 서비스 계정을 만들어 주세요",
            error.split(':').nth(1).unwrap_or("?")
        ),
        "resolve_failed" => format!("1Password 가 받지 않았어요 — {}", error.split_once(": ").map_or("", |(_, d)| d)),
        "helper_missing" => "이 판에 1Password 실행기(kasa-op)가 없어요. 실행기가 든 판으로 업데이트해 주세요".into(),
        "helper_unverified" => "1Password 실행기의 서명을 확인하지 못했어요".into(),
        "helper_timeout" => "1Password 에 닿지 못했어요. 연결 상태를 확인해 주세요".into(),
        "bad_key" => "열쇠 모양이 맞지 않아요. 폰에서 열쇠를 다시 만들어 주세요".into(),
        "isolated_run" => "검증 실행에서는 키체인을 쓰지 않아요".into(),
        "signed_out" => "KASA 계정에 로그인해야 해요".into(),
        "gateway_unreachable" => "관문에 닿지 못했어요".into(),
        "update_required" => "관문이 이 기능을 아직 몰라요. 관문 업데이트가 필요해요".into(),
        _ => "요청을 마치지 못했어요. 다시 시도해 주세요".into(),
    }
}

impl State {
    pub(crate) fn view(&self, signed_in: bool) -> View {
        View {
            signed_in: signed_in || self.fixture,
            status: self.status.clone(),
            untrusted: self.untrusted.clone(),
            confirm_clear: self.confirm_clear,
            busy: self.busy.is_some(),
            message: self.message.clone(),
        }
    }

    fn refresh_due(&mut self, force: bool) {
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
            let status = kasa_mcp::op_approval::status();
            let untrusted = kasa_mcp::op_approval::untrusted_keys().map_err(|e| e.to_string());
            let _ = tx.send(Done::Loaded(status, untrusted));
        });
    }

    /// 새로 맡겨진 폰 열쇠 — 알림으로 한 번 알린다(앱을 켤 때 이미 있던 것은 조용히).
    fn poll(&mut self) -> (bool, Vec<serde_json::Value>) {
        let mut changed = false;
        let mut fresh = Vec::new();
        if let Some(Ok(Done::Loaded(status, untrusted))) = self.fetch.as_ref().map(Receiver::try_recv) {
            self.fetch = None;
            changed = true;
            self.status = status;
            if let Ok(list) = untrusted {
                for key in &list {
                    let id = key["id"].as_str().unwrap_or_default().to_string();
                    if self.notified.insert(id) && self.loaded_once {
                        fresh.push(key.clone());
                    }
                }
                self.untrusted = list;
            }
            self.loaded_once = true;
        }
        if let Some(Ok(Done::Acted(result))) = self.busy.as_ref().map(Receiver::try_recv) {
            self.busy = None;
            changed = true;
            self.confirm_clear = false;
            self.message = Some(match result {
                Ok(message) => (message, false),
                Err(error) => (readable(&error), true),
            });
            self.fetched = None;
        }
        (changed, fresh)
    }

    fn run(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        self.message = None;
        std::thread::spawn(move || {
            let _ = tx.send(Done::Acted(work()));
        });
    }

    /// 검증 실행 화면: 토큰이 든 상태, 믿는 폰 하나, 새로 맡겨진 열쇠 하나.
    pub(crate) fn fixture(&mut self) {
        self.fixture = true;
        self.status = serde_json::json!({"token": true, "vault": "kasaterm-agents", "helper": true,
            "trusted": [{"label": "건호의 iPhone", "fingerprint": "3F2A-91C0-7D4E"}]});
        self.untrusted = vec![serde_json::json!({"device": "dev_ipad", "label": "건호의 iPad", "id": "b81c55e0aa04",
            "fingerprint": "B81C-55E0-AA04", "public": ""})];
    }

    pub(crate) fn hide(&mut self) {
        self.confirm_clear = false;
    }
}

impl App {
    pub(crate) fn op_approval_poll(&mut self) {
        if let Some(token) = TOKEN_ANSWER.lock().ok().and_then(|mut a| a.take()) {
            self.device_account.op.run(move || {
                kasa_mcp::op_approval::set_token(&token)
                    .map(|vault| format!("토큰을 넣었어요 · 금고 「{vault}」만 읽어요"))
                    .map_err(|e| e.to_string())
            });
        }
        if let Some(id) = TRUST_ANSWER.lock().ok().and_then(|mut a| a.take()) {
            let key = self.device_account.op.untrusted.iter().find(|k| k["id"] == id.as_str()).cloned();
            if let Some(key) = key {
                self.device_account.op.run(move || {
                    kasa_mcp::op_approval::trust(&key)
                        .map(|()| format!("{} 의 열쇠를 믿어요", key["label"].as_str().unwrap_or("폰")))
                        .map_err(|e| e.to_string())
                });
            }
        }
        self.op_autotrust();
        let op = &mut self.device_account.op;
        // 화면 상태(`logged_in`)는 설정을 열 때만 갱신된다 — 새 열쇠 알림은 설정을 안 열어도 떠야 하니
        // 로그인 여부는 뒷실 조회(로그인 없으면 빈 목록)가 판단한다.
        op.refresh_due(false);
        let (changed, fresh) = op.poll();
        for key in fresh {
            crate::chrome::notify_desktop(
                "새 Face ID 승인 열쇠",
                &format!(
                    "{} · {} — 설정 → 계정 → 1Password 에서 폰의 지문과 맞춰 보고 믿어 주세요",
                    key["label"].as_str().unwrap_or("폰"),
                    key["fingerprint"].as_str().unwrap_or("")
                ),
                None,
                Some(&format!("op-key:{}", key["id"].as_str().unwrap_or(""))),
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

    /// 검증 실행이 새 열쇠의 믿기 시트를 띄우고, 정한 시각에 찍은 뒤 [믿기]를 누른다 — 사람 누름과 같은 완료 처리기를 탄다.
    fn op_autotrust(&mut self) {
        if !crate::verification_run() {
            return;
        }
        let Some(ms) = std::env::var("KASATERM_OP_AUTOTRUST_MS").ok().and_then(|v| v.parse::<u64>().ok()) else { return };
        match self.device_account.op.auto {
            Some((handle, at)) if std::time::Instant::now() >= at => {
                if let Ok(path) = std::env::var("KASATERM_OP_AUTOTRUST_SHOT") {
                    shot_alert(handle, &path);
                }
                crate::remote_approval::press_sheet(handle, 0);
                self.device_account.op.auto = Some((0, at + std::time::Duration::from_secs(86_400)));
            }
            None => {
                let Some(key) = self.device_account.op.untrusted.first().cloned() else { return };
                let handle = self.window.as_ref().and_then(|w| trust_sheet(w, &key));
                if let Some(handle) = handle {
                    self.device_account.op.auto = Some((handle, std::time::Instant::now() + std::time::Duration::from_millis(ms)));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn op_approval_action(&mut self, act: Act) {
        if self.device_account.op.busy.is_some() {
            return;
        }
        self.device_account.op.message = None;
        match act {
            Act::AskToken => {
                if let Some(window) = &self.window {
                    token_sheet(window);
                }
            }
            Act::AskClear => self.device_account.op.confirm_clear = true,
            Act::KeepToken => self.device_account.op.confirm_clear = false,
            Act::Clear => {
                self.device_account.op.run(|| {
                    kasa_mcp::op_approval::clear_token().map(|()| "토큰을 지웠어요".to_string()).map_err(|e| e.to_string())
                });
            }
            Act::Trust(index) => {
                let Some(key) = self.device_account.op.untrusted.get(index).cloned() else { return };
                if let Some(window) = &self.window {
                    let _ = trust_sheet(window, &key);
                }
            }
            Act::Untrust(index) => {
                let list = kasa_mcp::op_approval::trusted().unwrap_or_default();
                let Some(t) = list.get(index).cloned() else { return };
                self.device_account.op.run(move || {
                    kasa_mcp::op_approval::untrust(&t.id)
                        .map(|()| format!("{} 의 열쇠를 더는 믿지 않아요", t.label))
                        .map_err(|e| e.to_string())
                });
            }
        }
        self.chrome_dirty = true;
    }
}

#[cfg(target_os = "macos")]
fn alert(window: &winit::window::Window, title: &str, info: &str) -> Option<(*mut objc2::runtime::AnyObject, *mut objc2::runtime::AnyObject)> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        let alert: *mut AnyObject = msg_send![AnyClass::get(c"NSAlert")?, new];
        if ns_window.is_null() || alert.is_null() {
            return None;
        }
        let _: () = msg_send![alert, setMessageText: &*NSString::from_str(title)];
        let _: () = msg_send![alert, setInformativeText: &*NSString::from_str(info)];
        Some((alert, ns_window))
    }
}

/// 토큰 넣기 — 암호 칸 하나. 넣은 글은 답 자리에만 잠깐 있고 칸은 바로 비운다.
#[cfg(target_os = "macos")]
fn token_sheet(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    let info = "학생용 전용 금고 하나만 「읽기」로 연 1Password 서비스 계정 토큰을 붙여 넣으세요. 이 맥 키체인에 kasaterm 만 읽게 넣고, 화면·기록·관문·폰에는 남기지 않아요.";
    let Some((alert, ns_window)) = alert(window, "1Password 서비스 계정 토큰", info) else { return };
    unsafe {
        let Some(class) = AnyClass::get(c"NSSecureTextField") else { return };
        let field: *mut AnyObject = msg_send![class, alloc];
        let field: *mut AnyObject = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(380.0, 24.0))];
        let _: () = msg_send![field, setPlaceholderString: &*NSString::from_str("ops_…")];
        let _: () = msg_send![alert, setAccessoryView: field];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("넣기")];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("취소")];
        let addr = (alert as usize, field as usize);
        let handler = block2::RcBlock::new(move |resp: isize| {
            let (alert, field) = (addr.0 as *mut AnyObject, addr.1 as *mut AnyObject);
            if resp == 1000 {
                let value: *mut NSString = msg_send![field, stringValue];
                if let Some(value) = value.as_ref() {
                    let token = value.to_string();
                    if let Ok(mut answer) = TOKEN_ANSWER.lock() {
                        *answer = Some(token);
                    }
                }
            }
            let _: () = msg_send![field, setStringValue: &*NSString::from_str("")];
            let _: () = msg_send![field, release];
            let _: () = msg_send![alert, release];
        });
        let _: () = msg_send![alert, beginSheetModalForWindow: ns_window, completionHandler: &*handler];
        let alert_window: *mut AnyObject = msg_send![alert, window];
        if !alert_window.is_null() {
            let _: bool = msg_send![alert_window, makeFirstResponder: field];
        }
    }
}

/// 폰 열쇠 믿기 — 두 화면의 지문을 사람이 맞춰 본다. Return 은 취소다(치던 Enter 가 새도 믿음이 나가지 않게).
#[cfg(target_os = "macos")]
fn trust_sheet(window: &winit::window::Window, key: &serde_json::Value) -> Option<usize> {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_foundation::NSString;
    let label = key["label"].as_str().unwrap_or("폰");
    let fingerprint = key["fingerprint"].as_str().unwrap_or("");
    let info = format!(
        "{label}\n지문  {fingerprint}\n\n폰 설정 → 계정 → Face ID 승인 열쇠에 같은 지문이 보이는지 맞춰 보세요. 믿으면 학생의 1Password 요청을 이 폰이 Face ID 로 한 번씩 허락할 수 있어요."
    );
    let (alert, ns_window) = alert(window, "이 폰의 Face ID 열쇠를 믿을까요?", &info)?;
    let id = key["id"].as_str().unwrap_or_default().to_string();
    unsafe {
        let trust: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("믿기")];
        let cancel: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("취소")];
        let _: () = msg_send![trust, setKeyEquivalent: &*NSString::from_str("")];
        let _: () = msg_send![cancel, setKeyEquivalent: &*NSString::from_str("\r")];
        let _: () = msg_send![trust, setHasDestructiveAction: Bool::YES];
        let addr = alert as usize;
        let handler = block2::RcBlock::new(move |resp: isize| {
            if resp == 1000 {
                if let Ok(mut answer) = TRUST_ANSWER.lock() {
                    *answer = Some(id.clone());
                }
            }
            let _: () = msg_send![addr as *mut AnyObject, release];
        });
        let _: () = msg_send![alert, beginSheetModalForWindow: ns_window, completionHandler: &*handler];
    }
    Some(alert as usize)
}

/// 검증 실행 전용 — 시트 창을 창 서버에서 뜬다. 뷰 비트맵(`cacheDisplayInRect`)은 시트의 글자를 빠뜨리고, 제 창 한 장은
/// 화면 녹화 권한 없이 `CGWindowListCreateImage` 로 뜰 수 있다(macos_sparkle.rs 업데이트 리그와 같은 길).
#[cfg(target_os = "macos")]
fn shot_alert(alert: usize, path: &str) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    type CreateImage = unsafe extern "C" fn(objc2_foundation::NSRect, u32, u32, u32) -> *mut AnyObject;
    unsafe extern "C" {
        fn CFRelease(cf: *const std::ffi::c_void);
    }
    unsafe {
        let window: *mut AnyObject = msg_send![alert as *mut AnyObject, window];
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"CGWindowListCreateImage".as_ptr());
        let (Some(rep_class), Some(dict_class)) = (AnyClass::get(c"NSBitmapImageRep"), AnyClass::get(c"NSDictionary")) else { return };
        if window.is_null() || symbol.is_null() {
            return;
        }
        let create: CreateImage = std::mem::transmute(symbol);
        let number: isize = msg_send![window, windowNumber];
        let null_rect = objc2_foundation::NSRect::new(
            objc2_foundation::NSPoint::new(f64::INFINITY, f64::INFINITY),
            objc2_foundation::NSSize::new(0.0, 0.0),
        );
        let image = create(null_rect, 8, number as u32, 1);
        if image.is_null() {
            return;
        }
        let rep: *mut AnyObject = msg_send![rep_class, alloc];
        let rep: *mut AnyObject = msg_send![rep, initWithCGImage: image];
        CFRelease(image.cast());
        if rep.is_null() {
            return;
        }
        let props: *mut AnyObject = msg_send![dict_class, dictionary];
        let data: *mut AnyObject = msg_send![rep, representationUsingType: 4usize, properties: props];
        if !data.is_null() {
            let len: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            let _ = std::fs::write(path, std::slice::from_raw_parts(bytes, len));
        }
        let _: () = msg_send![rep, release];
    }
}

#[cfg(not(target_os = "macos"))]
fn shot_alert(_alert: usize, _path: &str) {}

#[cfg(not(target_os = "macos"))]
fn token_sheet(_window: &winit::window::Window) {}

#[cfg(not(target_os = "macos"))]
fn trust_sheet(_window: &winit::window::Window, _key: &serde_json::Value) -> Option<usize> {
    None
}

fn act(a: Act) -> Target {
    Target::Setting(SettingsAction::DeviceAccount(Action::Op(a)))
}

pub(crate) fn paint(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    let v = &s.device_account.op;
    g.queue_icon("shield", x, *y - 1.0, 17.0, theme::text());
    draw_text(g, x + 24.0, *y, "1Password", 14.5, theme::text(), true);
    *y += 26.0;
    if !v.signed_in {
        info_slab(
            g,
            x,
            y,
            w,
            "KASA 계정에 로그인하면 학생이 1Password 전용 금고의 비밀을 폰 Face ID 한 번으로 읽게 할 수 있어요.",
        );
        *y += 14.0;
        return;
    }
    let has_token = v.status["token"] == true;
    let line = if has_token {
        format!("토큰 있음 · 금고 「{}」만 읽기", v.status["vault"].as_str().unwrap_or(""))
    } else if v.status.is_null() {
        "확인하는 중…".to_string()
    } else {
        "토큰 없음".to_string()
    };
    let line = fit(g, &line, w - 170.0, 12.0, false);
    draw_text(g, x, *y + 4.0, &line, 12.0, theme::text(), false);
    let helper_missing = v.status["helper"] == false;
    draw_text(
        g,
        x,
        *y + 21.0,
        if helper_missing { "이 판에 실행기가 없어요" } else { "키체인 · kasaterm 만 읽음" },
        10.5,
        if helper_missing { theme::danger() } else { theme::text_dim() },
        false,
    );
    let cy = *y + (ROW_H - CTL_H) / 2.0;
    if v.busy {
        draw_text(g, x + w - 64.0, cy + 5.0, "처리 중…", 12.0, theme::text_dim(), false);
    } else if v.confirm_clear {
        button(g, s, hits, (x + w - 150.0, cy, 82.0, CTL_H), "정말 지우기", act(Act::Clear), false);
        button(g, s, hits, (x + w - 60.0, cy, 60.0, CTL_H), "취소", act(Act::KeepToken), false);
    } else if has_token {
        button(g, s, hits, (x + w - 150.0, cy, 82.0, CTL_H), "바꾸기", act(Act::AskToken), false);
        button(g, s, hits, (x + w - 60.0, cy, 60.0, CTL_H), "지우기", act(Act::AskClear), false);
    } else {
        button(g, s, hits, (x + w - 96.0, cy, 96.0, CTL_H), "토큰 넣기", act(Act::AskToken), true);
    }
    *y += ROW_H;

    let trusted = v.status["trusted"].as_array().cloned().unwrap_or_default();
    if !trusted.is_empty() || !v.untrusted.is_empty() {
        draw_text(g, x, *y + 6.0, "Face ID 승인 열쇠", 11.0, theme::text_dim(), false);
        *y += 28.0;
    }
    for (index, key) in trusted.iter().enumerate() {
        let label = format!("{} · {}", key["label"].as_str().unwrap_or("폰"), key["fingerprint"].as_str().unwrap_or(""));
        let label = fit(g, &label, w - 80.0, 12.0, false);
        draw_text(g, x, *y + 4.0, &label, 12.0, theme::text(), false);
        draw_text(g, x, *y + 21.0, "믿음", 10.5, theme::text_dim(), false);
        if !v.busy {
            button(g, s, hits, (x + w - 64.0, *y + (ROW_H - CTL_H) / 2.0, 64.0, CTL_H), "거두기", act(Act::Untrust(index)), false);
        }
        *y += ROW_H;
    }
    for (index, key) in v.untrusted.iter().enumerate() {
        let label = format!("{} · {}", key["label"].as_str().unwrap_or("폰"), key["fingerprint"].as_str().unwrap_or(""));
        let label = fit(g, &label, w - 80.0, 12.0, true);
        draw_text(g, x, *y + 4.0, &label, 12.0, theme::text(), true);
        draw_text(g, x, *y + 21.0, "새 열쇠 — 폰의 지문과 맞춰 보고 믿기", 10.5, theme::text_dim(), false);
        if !v.busy {
            button(g, s, hits, (x + w - 64.0, *y + (ROW_H - CTL_H) / 2.0, 64.0, CTL_H), "믿기…", act(Act::Trust(index)), true);
        }
        *y += ROW_H;
    }
    info_slab(
        g,
        x,
        y,
        w,
        if trusted.is_empty() {
            "폰 설정 → 계정 → Face ID 승인 열쇠를 만들면 여기 새 열쇠로 떠요. 믿기를 누른 폰만 학생의 1Password 요청을 허락할 수 있어요."
        } else {
            "학생은 kasaterm-cli op run -e 이름=op://… -- 명령 으로 써요. 요청마다 폰 Face ID 한 번, 값은 관문·폰을 지나지 않아요."
        },
    );
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

    #[test]
    fn only_keys_that_arrive_after_the_first_look_notify() {
        let mut state = State::default();
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Loaded(serde_json::json!({"token": true}), Ok(vec![serde_json::json!({"id": "old"})]))).unwrap();
        assert!(state.poll().1.is_empty(), "a key from before the app started notified");
        let (tx, rx) = mpsc::channel();
        state.fetch = Some(rx);
        tx.send(Done::Loaded(serde_json::json!({}), Ok(vec![serde_json::json!({"id": "old"}), serde_json::json!({"id": "new"})])))
            .unwrap();
        let fresh = state.poll().1;
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0]["id"], "new");
    }

    #[test]
    fn errors_read_as_plain_words() {
        assert!(readable("vault_count:3").contains("금고 3개"));
        assert!(readable("resolve_failed: forbidden").contains("forbidden"));
    }
}
