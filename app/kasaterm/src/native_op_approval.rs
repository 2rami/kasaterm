//! 설정 「연결 기기」의 「폰」 — 학생이 이 맥 키체인(Windows 는 자격 증명 관리자)의 비밀을 쓸 때, 새 기기가 로그인 금고
//! 열쇠를 받을 때 Face ID 로 허락할 폰(docs/secret-faceid-approval.md·docs/login-vault-sync.md). 폰은 기기라 PC 와
//! 같은 페이지에 둔다. `op_approval::auto_pick` 규칙대로 알아서 믿고, 그 밖의 것만 묻는다. 자동으로 믿은 폰은 비밀
//! 허락만 하고, 새 기기 들이기는 사람이 지문을 맞춰 보고 직접 믿어야 한다. 이름의 `op` 는 1Password 로 읽던 때의 것이다.

use super::*;
use std::collections::HashSet;
use std::sync::Mutex;

const REFRESH: std::time::Duration = std::time::Duration::from_secs(60);

/// 비밀을 꺼내는 자리 — (이 기기, 보관함). 맥은 키체인, Windows 는 자격 증명 관리자의 일반 자격 증명이다.
#[cfg(not(windows))]
const STORE: (&str, &str) = ("이 맥", "키체인");
#[cfg(windows)]
const STORE: (&str, &str) = ("이 PC", "자격 증명 관리자");
/// 처음 꺼낼 때 일 — 맥은 접근 허용 창이 뜨고, Windows 는 묻지 않는 대신 넣는 길을 알린다.
#[cfg(not(windows))]
const FIRST_READ: &str = "처음 꺼내는 항목은 이 맥에 접근 허용 창이 떠요";
#[cfg(windows)]
const FIRST_READ: &str = "일반 자격 증명만 읽어요 · cmdkey /generic:서비스 /user:계정 /pass 로 넣어요";
#[cfg(not(windows))]
const TRUSTS_FIRST_PHONE: &str = "폰 설정 → 연결 기기 → 비밀 승인에서 「Face ID 로 연결」을 누르면 이 맥이 첫 폰을 바로 믿어요.";
#[cfg(windows)]
const TRUSTS_FIRST_PHONE: &str = "폰 설정 → 연결 기기 → 비밀 승인에서 「Face ID 로 연결」을 누르면 이 PC가 첫 폰을 바로 믿어요.";
#[cfg(not(windows))]
const ISOLATED: &str = "검증 실행에서는 키체인을 쓰지 않아요";
#[cfg(windows)]
const ISOLATED: &str = "검증 실행에서는 자격 증명 관리자를 쓰지 않아요";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Trust(usize),
    Untrust(usize),
}

#[derive(Debug)]
enum Done {
    Loaded(serde_json::Value, Result<Vec<serde_json::Value>, String>),
    Acted(Result<String, String>),
}

/// 시트의 답 — 완료 처리기는 App 을 못 잡으니 여기 두고 다음 poll 이 꺼내 간다.
static TRUST_ANSWER: Mutex<Option<String>> = Mutex::new(None);

#[derive(Default)]
pub(crate) struct State {
    status: serde_json::Value,
    untrusted: Vec<serde_json::Value>,
    fetched: Option<std::time::Instant>,
    fetch: Option<Receiver<Done>>,
    busy: Option<Receiver<Done>>,
    message: Option<(String, bool)>,
    notified: HashSet<String>,
    believed: HashSet<String>,
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
    busy: bool,
    message: Option<(String, bool)>,
}

fn readable(error: &str) -> String {
    let code = error.split(':').next().unwrap_or(error).trim();
    match code {
        "bad_key" => "열쇠 모양이 맞지 않아요. 폰에서 열쇠를 다시 만들어 주세요".into(),
        "isolated_run" => ISOLATED.into(),
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
            let done = match kasa_device::op_approval::sync() {
                Ok((status, rest)) => Done::Loaded(status, Ok(rest)),
                Err(error) => Done::Loaded(kasa_device::op_approval::status(), Err(error.to_string())),
            };
            let _ = tx.send(done);
        });
    }

    /// 새로 맡겨진 폰 열쇠(사람이 믿어야 할 것)와 새로 믿게 된 열쇠 — 알림으로 한 번씩 알린다(앱을 켤 때 이미 있던 것은 조용히).
    fn poll(&mut self) -> (bool, Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let mut changed = false;
        let mut fresh = Vec::new();
        let mut believed = Vec::new();
        if let Some(Ok(Done::Loaded(status, untrusted))) = self.fetch.as_ref().map(Receiver::try_recv) {
            self.fetch = None;
            changed = true;
            for key in status["trusted"].as_array().into_iter().flatten() {
                let print = key["fingerprint"].as_str().unwrap_or_default().to_string();
                if self.believed.insert(print) && self.loaded_once {
                    believed.push(key.clone());
                }
            }
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
        match self.busy.as_ref().map(Receiver::try_recv) {
            Some(Ok(Done::Acted(result))) => {
                self.busy = None;
                changed = true;
                self.message = Some(match result {
                    Ok(message) => (message, false),
                    Err(error) => (readable(&error), true),
                });
                self.fetched = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.busy = None;
                changed = true;
            }
            _ => {}
        }
        if kasa_device::op_approval::take_changed() {
            self.fetched = None;
        }
        (changed, fresh, believed)
    }

    fn run(&mut self, work: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        self.message = None;
        std::thread::spawn(move || {
            let _ = tx.send(Done::Acted(work()));
        });
    }

    /// 검증 실행 화면: 직접 믿은 폰·자동으로 믿은 폰 하나씩, 새로 맡겨진 열쇠 하나. [empty] 면 처음 연결 전(열쇠 없음).
    pub(crate) fn fixture(&mut self, empty: bool) {
        self.fixture = true;
        if empty {
            self.status = serde_json::json!({"source": "keychain", "token": true, "trusted": []});
            return;
        }
        self.status = serde_json::json!({"source": "keychain", "token": true,
            "trusted": [{"label": "내 iPhone", "fingerprint": "3F2A-91C0-7D4E", "id": "a1", "manual": true},
                        {"label": "작업 iPad", "fingerprint": "7B0E-44A2-19C3", "id": "a2", "manual": false}]});
        self.untrusted = vec![serde_json::json!({"device": "dev_ipad", "label": "내 iPad", "id": "b81c55e0aa04",
            "fingerprint": "B81C-55E0-AA04", "public": ""})];
    }

}

impl App {
    pub(crate) fn op_approval_poll(&mut self) {
        if let Some(id) = TRUST_ANSWER.lock().ok().and_then(|mut a| a.take()) {
            let key = self.device_account.op.untrusted.iter().find(|k| k["id"] == id.as_str()).cloned();
            if let Some(key) = key {
                self.device_account.op.run(move || {
                    kasa_device::op_approval::trust(&key)
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
        let (changed, fresh, believed) = op.poll();
        for key in believed {
            crate::chrome::notify_desktop(
                "비밀 승인 폰을 믿어요",
                &format!(
                    "{} — 학생의 {} 비밀 요청은 이제 이 폰에서 Face ID 로 허락해요",
                    key["label"].as_str().unwrap_or("폰"),
                    STORE.1
                ),
                None,
                Some(&format!("op-trusted:{}", key["fingerprint"].as_str().unwrap_or(""))),
                None,
            );
        }
        for key in fresh {
            crate::chrome::notify_desktop(
                "새 Face ID 승인 열쇠",
                &format!(
                    "{} · {} — 설정 → 연결 기기 → 폰에서 폰의 지문과 맞춰 보고 넣어 주세요",
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
                press_sheet(handle, 0);
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
            Act::Trust(index) => {
                let Some(key) = self.device_account.op.untrusted.get(index).cloned() else { return };
                if let Some(window) = &self.window {
                    let _ = trust_sheet(window, &key);
                }
            }
            Act::Untrust(index) => {
                let list = kasa_device::op_approval::trusted().unwrap_or_default();
                let Some(t) = list.get(index).cloned() else { return };
                self.device_account.op.run(move || {
                    kasa_device::op_approval::untrust(&t.id)
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

const TRUST_TITLE: &str = "이 폰의 Face ID 열쇠를 믿을까요?";

fn trust_info(key: &serde_json::Value) -> String {
    let label = key["label"].as_str().unwrap_or("폰");
    let fingerprint = key["fingerprint"].as_str().unwrap_or("");
    format!(
        "{label}\n지문  {fingerprint}\n\n폰 설정 → 계정 → Face ID 승인 열쇠에 같은 지문이 보이는지 맞춰 보세요. 믿으면 학생의 {} 비밀 요청을 이 폰이 Face ID 로 한 번씩 허락할 수 있어요.",
        STORE.1
    )
}

/// 폰 열쇠 믿기 — 두 화면의 지문을 사람이 맞춰 본다.
fn trust_sheet(window: &winit::window::Window, key: &serde_json::Value) -> Option<usize> {
    let id = key["id"].as_str().unwrap_or_default().to_string();
    confirm_sheet(window, TRUST_TITLE, &trust_info(key), "믿기", &TRUST_ANSWER, id)
}

/// 사람이 맞춰 보고 누르는 확인 창. 누르면 `answer` 에 `value` 를 두고 다음 poll 이 꺼내 간다.
/// Return 은 취소다(치던 Enter 가 새도 믿음이 나가지 않게).
pub(crate) fn confirm_sheet(
    window: &winit::window::Window,
    title: &str,
    info: &str,
    primary: &str,
    answer: &'static Mutex<Option<String>>,
    value: String,
) -> Option<usize> {
    let spec = SheetSpec { title, info, body: None, buttons: &[primary, "취소"], primary_enabled: true };
    decision_sheet(window, &spec, move |choice| {
        if choice == Some(0) {
            if let Ok(mut slot) = answer.lock() {
                *slot = Some(value.clone());
            }
        }
    })
}

/// 결정 시트 한 장. 단추 0 이 주 동작(오른쪽 끝, 위험 표시), 1 이 Return 을 받는 기본 동작이고 그 뒤 단추는 왼쪽으로 선다.
pub(crate) struct SheetSpec<'a> {
    pub(crate) title: &'a str,
    pub(crate) info: &'a str,
    /// 고정폭 읽기 전용 원문 칸 — 길면 칸 안에서 스크롤한다(시트가 화면 밖으로 자라지 않게).
    pub(crate) body: Option<&'a str>,
    pub(crate) buttons: &'a [&'a str],
    pub(crate) primary_enabled: bool,
}

/// 원문 칸 폭·높이 상한(맥·Windows 같다).
#[cfg(target_os = "macos")]
const BODY_W: f64 = 560.0;
#[cfg(target_os = "macos")]
const BODY_MAX_H: f64 = 260.0;

/// 시트를 띄우고 손잡이를 돌려준다. `answer` 는 누른 단추 차례로 한 번 불리고, 결정 없이 거두면 None 이다.
#[cfg(target_os = "macos")]
pub(crate) fn decision_sheet(
    window: &winit::window::Window,
    spec: &SheetSpec,
    answer: impl Fn(Option<usize>) + 'static,
) -> Option<usize> {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    use objc2_foundation::NSString;
    let (alert, ns_window) = alert(window, spec.title, spec.info)?;
    let count = spec.buttons.len();
    unsafe {
        if let Some(body) = spec.body {
            let view = body_view(body);
            if !view.is_null() {
                let _: () = msg_send![alert, setAccessoryView: view];
                let _: () = msg_send![view, release];
            }
        }
        for (index, label) in spec.buttons.iter().enumerate() {
            let button: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str(label)];
            let key = if index == 1 { "\r" } else { "" };
            let _: () = msg_send![button, setKeyEquivalent: &*NSString::from_str(key)];
            if index == 0 {
                let _: () = msg_send![button, setHasDestructiveAction: Bool::YES];
                if !spec.primary_enabled {
                    let _: () = msg_send![button, setEnabled: Bool::NO];
                }
            }
        }
        let addr = alert as usize;
        let handler = block2::RcBlock::new(move |resp: isize| {
            // NSAlertFirstButtonReturn = 1000, 다음 단추 1001…. `close_sheet` 로 거두면 0 이다.
            answer(usize::try_from(resp - 1000).ok().filter(|index| *index < count));
            let _: () = msg_send![addr as *mut AnyObject, release];
        });
        let _: () = msg_send![alert, beginSheetModalForWindow: ns_window, completionHandler: &*handler];
        Some(addr)
    }
}

/// 원문 칸 — 고정폭 읽기 전용, 고르기·복사 가능. 높이는 글에 맞추되 상한을 넘으면 칸 안에서 스크롤한다.
#[cfg(target_os = "macos")]
unsafe fn body_view(body: &str) -> *mut objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    let (Some(scroll_class), Some(text_class), Some(font_class)) =
        (AnyClass::get(c"NSScrollView"), AnyClass::get(c"NSTextView"), AnyClass::get(c"NSFont"))
    else {
        return std::ptr::null_mut();
    };
    unsafe {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(BODY_W, BODY_MAX_H));
        let text: *mut AnyObject = msg_send![text_class, alloc];
        let text: *mut AnyObject = msg_send![text, initWithFrame: frame];
        let _: () = msg_send![text, setString: &*NSString::from_str(body)];
        let _: () = msg_send![text, setEditable: Bool::NO];
        let _: () = msg_send![text, setSelectable: Bool::YES];
        let _: () = msg_send![text, setVerticallyResizable: Bool::YES];
        let _: () = msg_send![text, setAutoresizingMask: 2usize];
        let _: () = msg_send![text, setTextContainerInset: NSSize::new(4.0, 6.0)];
        let font: *mut AnyObject = msg_send![font_class, monospacedSystemFontOfSize: 12.0f64, weight: 0.0f64];
        if !font.is_null() {
            let _: () = msg_send![text, setFont: font];
        }
        let layout: *mut AnyObject = msg_send![text, layoutManager];
        let container: *mut AnyObject = msg_send![text, textContainer];
        let mut height = BODY_MAX_H;
        if !layout.is_null() && !container.is_null() {
            let _: () = msg_send![layout, ensureLayoutForTextContainer: container];
            let used: NSRect = msg_send![layout, usedRectForTextContainer: container];
            height = (used.size.height + 12.0 + 4.0).clamp(48.0, BODY_MAX_H);
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(BODY_W, height));
        let scroll: *mut AnyObject = msg_send![scroll_class, alloc];
        let scroll: *mut AnyObject = msg_send![scroll, initWithFrame: frame];
        let _: () = msg_send![scroll, setHasVerticalScroller: Bool::YES];
        let _: () = msg_send![scroll, setBorderType: 2isize];
        let _: () = msg_send![scroll, setDocumentView: text];
        let _: () = msg_send![text, release];
        scroll
    }
}

/// 다른 곳에서 끝난 요청의 시트를 결정 없이 거둔다.
#[cfg(target_os = "macos")]
pub(crate) fn close_sheet(alert: usize) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    unsafe {
        let sheet: *mut AnyObject = msg_send![alert as *mut AnyObject, window];
        if sheet.is_null() {
            return;
        }
        let parent: *mut AnyObject = msg_send![sheet, sheetParent];
        if !parent.is_null() {
            let _: () = msg_send![parent, endSheet: sheet, returnCode: 0isize];
        }
    }
}

/// 검증 실행 전용 — 시트 창을 창 서버에서 뜬다. 뷰 비트맵(`cacheDisplayInRect`)은 시트의 글자를 빠뜨리고, 제 창 한 장은
/// 화면 녹화 권한 없이 `CGWindowListCreateImage` 로 뜰 수 있다(macos_sparkle.rs 업데이트 리그와 같은 길).
#[cfg(target_os = "macos")]
pub(crate) fn shot_alert(alert: usize, path: &str) {
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

/// 맥 시트와 같은 글·단추 — Enter 는 기본 동작(단추 1).
#[cfg(windows)]
pub(crate) fn decision_sheet(
    window: &winit::window::Window,
    spec: &SheetSpec,
    answer: impl Fn(Option<usize>) + 'static,
) -> Option<usize> {
    let spec = crate::win_sheet::Spec {
        title: spec.title,
        info: spec.info,
        body: spec.body,
        buttons: spec.buttons,
        primary_enabled: spec.primary_enabled,
    };
    crate::win_sheet::open(window, &spec, answer)
}

#[cfg(windows)]
pub(crate) fn close_sheet(alert: usize) {
    crate::win_sheet::close(alert);
}

#[cfg(windows)]
pub(crate) fn shot_alert(alert: usize, path: &str) {
    crate::win_sheet::shot(alert, path);
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn shot_alert(_alert: usize, _path: &str) {}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn decision_sheet(
    _window: &winit::window::Window,
    _spec: &SheetSpec,
    _answer: impl Fn(Option<usize>) + 'static,
) -> Option<usize> {
    None
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn close_sheet(_alert: usize) {}

fn act(a: Act) -> Target {
    Target::Setting(SettingsAction::DeviceAccount(Action::Op(a)))
}

/// 「연결 기기」의 「폰」 묶음. 줄마다 폰 이름과 허락 범위, 오른쪽 단추.
pub(crate) fn paint_phones(g: &mut gpu::GpuRenderer, s: &Snapshot, hits: &mut Vec<Hit>, x: f32, y: &mut f32, w: f32) {
    let v = &s.device_account.op;
    section_title(g, x, *y, "폰", "");
    *y += 54.0;
    if !v.signed_in {
        plain_hint(g, x, y, w, &format!("KASA 계정에 로그인하면 학생이 {} {}의 비밀을 쓸 때 폰 Face ID 로 허락할 수 있어요.", STORE.0, STORE.1));
        return;
    }
    let width = |g: &mut gpu::GpuRenderer, label: &str| crate::native_controls::button_width(g, label, CTL_H, true);
    let line = |g: &mut gpu::GpuRenderer, hits: &mut Vec<Hit>, y: &mut f32, name: &str, bold: bool, dim: &str, buttons: &[(&str, Act, bool)]| {
        let mut right = x + w;
        let mut reserve = 0.0;
        for (label, a, primary) in buttons.iter().rev() {
            let bw = width(g, label);
            right -= bw;
            if !v.busy {
                button(g, s, hits, (right, *y + (ROW_H - CTL_H) / 2.0, bw, CTL_H), label, act(*a), *primary);
            }
            right -= 8.0;
            reserve += bw + 8.0;
        }
        let name = fit(g, name, w - reserve - 12.0, 12.0, bold);
        draw_text(g, x, *y + 4.0, &name, 12.0, theme::text(), bold);
        let dim = fit(g, dim, w - reserve - 12.0, 10.5, false);
        draw_text(g, x, *y + 21.0, &dim, 10.5, theme::text_dim(), false);
        *y += ROW_H;
    };
    let trusted = v.status["trusted"].as_array().cloned().unwrap_or_default();
    for (index, key) in trusted.iter().enumerate() {
        let name = key["label"].as_str().unwrap_or("폰");
        let print = key["fingerprint"].as_str().unwrap_or("");
        line(g, hits, y, name, false, &format!("Face ID 로 허락 · 비밀 쓰기 · 지문 {print}"), &[("빼기", Act::Untrust(index), false)]);
    }
    for (index, key) in v.untrusted.iter().enumerate() {
        let name = format!("{} · 새 열쇠", key["label"].as_str().unwrap_or("폰"));
        let print = key["fingerprint"].as_str().unwrap_or("");
        line(g, hits, y, &name, true, &format!("폰 설정의 열쇠 지문과 맞춰 보고 넣어요 · 지문 {print}"), &[("넣기…", Act::Trust(index), true)]);
    }
    if trusted.is_empty() && v.untrusted.is_empty() {
        plain_hint(g, x, y, w, if v.status.is_null() { "확인하는 중…" } else { TRUSTS_FIRST_PHONE });
    } else {
        plain_hint(g, x, y, w, &format!("폰에서 Face ID로 이 기기의 비밀 요청을 허락할 수 있어요. {FIRST_READ}."));
    }
    if let Some((message, error)) = &v.message {
        for line in wrap_words(g, message, w, 10.5) {
            draw_text(g, x, *y, &line, 10.5, if *error { theme::danger() } else { theme::text_dim() }, false);
            *y += 18.0;
        }
    }
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
        assert!(readable("bad_key").contains("열쇠"));
        assert!(readable("gateway_unreachable").contains("관문"));
    }
}

/// 검증 실행이 시트 단추를 누른다 — 단추 차례(0 이 주 동작).
#[cfg(target_os = "macos")]
pub(crate) fn press_sheet(alert: usize, index: usize) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    unsafe {
        let alert = alert as *mut AnyObject;
        let buttons: *mut AnyObject = msg_send![alert, buttons];
        if buttons.is_null() {
            return;
        }
        let button: *mut AnyObject = msg_send![buttons, objectAtIndex: index];
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![button, performClick: nil];
    }
}

#[cfg(windows)]
pub(crate) fn press_sheet(alert: usize, index: usize) {
    crate::win_sheet::press(alert, index);
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn press_sheet(_alert: usize, _index: usize) {}
