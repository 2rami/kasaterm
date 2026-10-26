//! 원격 요청은 OS 알림으로 알리고, 사람이 누른 요청의 원문을 네이티브 승인 시트로 연다.
//! 허락·거절은 시트에서만 보내며, 비밀 요청은 폰 Face ID 경로에 남긴다.

use super::*;
use std::collections::HashSet;

/// pane id(`%N`)·새 판 센티널과 겹치지 않는 결정 알림 센티널.
pub(crate) const ACTION: &str = "__kasaterm_remote_approval__";

enum Msg {
    List(serde_json::Value),
}

#[derive(Default)]
pub(crate) struct State {
    started: bool,
    rx: Option<std::sync::mpsc::Receiver<Msg>>,
    items: Vec<serde_json::Value>,
    /// 이 기기의 관문 기기 id — 이 기기에서 난 요청을 가린다.
    me: Option<String>,
    announced: HashSet<String>,
    /// 열린 시트 — (요청 id, 지문, 시트 손잡이).
    sheet: Option<(String, String, usize)>,
    busy: Option<std::sync::mpsc::Receiver<Result<serde_json::Value, String>>>,
    /// 검증 실행 전용 — 시트가 선 뒤 이 시각에 그 단추를 누른다(`KASATERM_AUTOSHEET=allow|deny`). 사람 누름과 같은
    /// 완료 처리를 탄다. 검증 실행(`KASATERM_WINDOW_SIZE`) 밖에서는 서지 않는다.
    auto: Option<(std::time::Instant, bool)>,
}

fn notification_can_open(a: &serde_json::Value, me: Option<&str>) -> bool {
    a["state"] == "pending" && a["kind"] != "secret" && me != a["device"].as_str()
}

impl State {
    fn find(&self, id: &str) -> Option<&serde_json::Value> {
        self.items.iter().find(|a| a["id"] == id)
    }

    fn pending(&self, id: &str) -> bool {
        self.find(id).is_some_and(|a| a["state"] == "pending")
    }
}

/// 관문 목록을 긴 폴링으로 받는 뒷실. 로그인 전·격리 실행이면 30초마다 다시 본다.
fn spawn_poller(proxy: EventLoopProxy<UserEvent>) -> std::sync::mpsc::Receiver<Msg> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("remote-approval".into())
        .spawn(move || {
            let mut since: Option<u64> = None;
            let mut backoff = 2;
            loop {
                match kasa_mcp::device_auth::approvals::list_blocking(since, if since.is_some() { 25 } else { 0 }) {
                    Ok(value) => {
                        backoff = 2;
                        since = value["rev"].as_u64();
                        if tx.send(Msg::List(value)).is_err() {
                            return;
                        }
                        let _ = proxy.send_event(UserEvent::Redraw);
                    }
                    Err(error) => {
                        let code = error.to_string();
                        since = None;
                        let wait = if matches!(code.as_str(), "isolated_run" | "signed_out" | "update_required") { 30 } else { backoff };
                        backoff = (backoff * 2).min(30);
                        std::thread::sleep(std::time::Duration::from_secs(wait));
                    }
                }
            }
        })
        .ok();
    rx
}

/// 시트 답(요청 id, 허락이면 Some(true), 거절 Some(false), 닫힘 None) — 주 스레드 블록이 넣고 틱이 꺼낸다.
static ANSWERS: std::sync::Mutex<Vec<(String, Option<bool>)>> = std::sync::Mutex::new(Vec::new());

fn who(a: &serde_json::Value) -> String {
    a["student"].as_str().filter(|s| !s.is_empty()).unwrap_or("학생").to_string()
}

/// 알림 한 줄 — 「유우카 승인 요청 · 맥미니 · Bash · rm -rf build」.
fn notice_text(a: &serde_json::Value) -> String {
    let first = a["fields"][0]["text"].as_str().unwrap_or("");
    let line: String = first.lines().next().unwrap_or("").chars().take(80).collect();
    format!(
        "{} 승인 요청 · {} · {} · {}",
        who(a),
        a["machine"].as_str().unwrap_or(""),
        a["tool"].as_str().unwrap_or(""),
        line
    )
}

/// 시트 본문 — 칸마다 이름표 줄과 원문 전부. 비밀은 요청한 기기·관문이 이미 가렸다.
fn sheet_body(a: &serde_json::Value) -> String {
    let mut out = String::new();
    if let Some(cwd) = a["cwd"].as_str().filter(|c| !c.is_empty()) {
        out.push_str(&format!("[폴더]\n{cwd}\n\n"));
    }
    for f in a["fields"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
        out.push_str(&format!("[{}]\n{}\n\n", f["label"].as_str().unwrap_or(""), f["text"].as_str().unwrap_or("")));
    }
    if a["truncated"] == true {
        out.push_str("— 원문이 길어 앞부분만 실렸어요. 허락은 원래 창에서만 할 수 있어요.\n");
    }
    out.trim_end().to_string()
}

fn closed_line(a: &serde_json::Value) -> String {
    let by = a["by"]["label"].as_str().unwrap_or("다른 기기");
    let what = match a["state"].as_str().unwrap_or("") {
        "allowed" => format!("{by}에서 허락했어요"),
        "denied" => format!("{by}에서 거절했어요"),
        "expired" => "2분이 지나 원래 창으로 돌아갔어요".to_string(),
        _ => "원래 창에서 답했어요".to_string(),
    };
    format!("{} 요청 · {what}", who(a))
}

impl App {
    /// 주 스레드 루프 턴마다(`about_to_wait`) — 목록을 받아 알림을 세우고, 시트 답을 관문에 보낸다.
    pub(crate) fn tick_remote_approval(&mut self) {
        if self.lite {
            return;
        }
        if !self.remote_approval.started {
            self.remote_approval.started = true;
            self.remote_approval.rx = Some(spawn_poller(self.proxy.clone()));
            kasa_mcp::approval_bridge::spawn();
        }
        let mut lists = Vec::new();
        if let Some(rx) = &self.remote_approval.rx {
            while let Ok(Msg::List(value)) = rx.try_recv() {
                lists.push(value);
            }
        }
        if let Some(value) = lists.pop() {
            self.apply_remote_approvals(value);
        }
        if let (Some((at, allow)), Some((_, _, handle))) = (self.remote_approval.auto, self.remote_approval.sheet.as_ref()) {
            if std::time::Instant::now() >= at {
                self.remote_approval.auto = None;
                if let Ok(path) = std::env::var("KASATERM_AUTOSHEET_SHOT") {
                    shot_sheet(*handle, &path);
                }
                press_sheet(*handle, if allow { 0 } else { 1 });
            }
        }
        self.drain_remote_answers();
        self.drain_remote_decision();
    }

    fn apply_remote_approvals(&mut self, value: serde_json::Value) {
        let st = &mut self.remote_approval;
        if st.me.is_none() {
            st.me = kasa_mcp::device_auth::approvals::device_id();
        }
        st.items = value["approvals"].as_array().cloned().unwrap_or_default();
        let mut fresh = Vec::new();
        for a in &st.items {
            let Some(id) = a["id"].as_str() else { continue };
            // 키체인 비밀 요청은 폰 Face ID 서명으로만 허락된다 — 맥 시트의 [허락]은 관문이 받지 않는다.
            if a["state"] != "pending" || st.me.as_deref() == a["device"].as_str() || a["kind"] == "secret" {
                continue;
            }
            if st.announced.insert(id.to_string()) {
                fresh.push(a.clone());
            }
        }
        for a in &fresh {
            let id = a["id"].as_str().unwrap_or_default();
            crate::chrome::notify_desktop(
                &format!("{} · 승인 요청", who(a)), &notice_text(a), a["student"].as_str(),
                Some(&format!("remote-approval:{id}")), Some((ACTION, Some(id))),
            );
        }
        let mut closed = None;
        if let Some((id, _, handle)) = st.sheet.clone() {
            if !st.pending(&id) {
                st.sheet = None;
                close_sheet(handle);
                closed = closed.or_else(|| st.find(&id).cloned());
            }
        }
        if let Some(a) = closed {
            self.set_toast(closed_line(&a));
        }
        self.chrome_dirty = true;
    }

    pub(crate) fn open_remote_notification(&mut self, id: &str) {
        let valid = self.remote_approval.find(id).is_some_and(|a| {
            notification_can_open(a, self.remote_approval.me.as_deref())
        });
        if valid && self.remote_approval.sheet.is_none() { self.open_remote_sheet(id); }
    }

    fn open_remote_sheet(&mut self, id: &str) {
        let Some(a) = self.remote_approval.find(id).filter(|a| a["state"] == "pending").cloned() else { return };
        let digest = a["digest"].as_str().unwrap_or_default().to_string();
        let left = a["expires"].as_u64().unwrap_or(0).saturating_sub(a["now"].as_u64().unwrap_or(0)) / 1000;
        let info = format!(
            "{} · {}{} · {}\n이 요청 한 번만이에요. 약 {}초 뒤 원래 창으로 돌아가요.",
            a["machine"].as_str().unwrap_or(""),
            who(&a),
            a["pane"].as_str().map(|p| format!(" ({p})")).unwrap_or_default(),
            a["tool"].as_str().unwrap_or(""),
            left
        );
        let title = format!("{} 승인 요청", who(&a));
        let allow = a["truncated"] != true;
        let key = id.to_string();
        let handle = self.window.as_ref().and_then(|w| {
            open_sheet(w, &title, &info, &sheet_body(&a), allow, move |answer| {
                if let Ok(mut q) = ANSWERS.lock() {
                    q.push((key.clone(), answer));
                }
            })
        });
        if let Some(handle) = handle {
            self.remote_approval.sheet = Some((id.to_string(), digest, handle));
            if crate::verification_run() {
                let ms = std::env::var("KASATERM_AUTOSHEET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
                self.remote_approval.auto = match std::env::var("KASATERM_AUTOSHEET").as_deref() {
                    Ok("allow") => Some((std::time::Instant::now() + std::time::Duration::from_millis(ms), true)),
                    Ok("deny") => Some((std::time::Instant::now() + std::time::Duration::from_millis(ms), false)),
                    _ => None,
                };
            }
        }
    }

    fn drain_remote_answers(&mut self) {
        let answers: Vec<_> = ANSWERS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (id, answer) in answers {
            let Some((open, digest, _)) = self.remote_approval.sheet.clone() else { continue };
            if open != id {
                continue;
            }
            self.remote_approval.sheet = None;
            let Some(allow) = answer else { continue };
            if self.remote_approval.busy.is_some() {
                continue;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            self.remote_approval.busy = Some(rx);
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let result = kasa_mcp::device_auth::approvals::decide_blocking(&id, &digest, allow).map_err(|error| {
                    match kasa_mcp::device_auth::approvals::closed_view(&error) {
                        Some(view) => format!("closed:{view}"),
                        None => error.to_string(),
                    }
                });
                let _ = tx.send(result);
                let _ = proxy.send_event(UserEvent::Redraw);
            });
        }
    }

    fn drain_remote_decision(&mut self) {
        let Some(Ok(result)) = self.remote_approval.busy.as_ref().map(|rx| rx.try_recv()) else { return };
        self.remote_approval.busy = None;
        let message = match result {
            Ok(a) => match a["state"].as_str() {
                Some("allowed") => format!("{} 요청을 허락했어요", who(&a)),
                Some("denied") => format!("{} 요청을 거절했어요", who(&a)),
                _ => "처리했어요".to_string(),
            },
            Err(error) => match error.strip_prefix("closed:").and_then(|v| serde_json::from_str(v).ok()) {
                Some(view) => closed_line(&view),
                None => match error.as_str() {
                    "digest_mismatch" => "보는 동안 요청이 바뀌었어요 · 다시 열어 확인해 주세요".to_string(),
                    "gateway_unreachable" => "관문에 닿지 못했어요 · 원래 창에서 답해 주세요".to_string(),
                    other => format!("결정을 못 보냈어요 · {other}"),
                },
            },
        };
        self.set_toast(message);
    }
}

#[cfg(target_os = "macos")]
fn open_sheet(
    window: &winit::window::Window,
    title: &str,
    info: &str,
    body: &str,
    allow_enabled: bool,
    answer: impl Fn(Option<bool>) + 'static,
) -> Option<usize> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return None;
        }
        let alert: *mut AnyObject = msg_send![AnyClass::get(c"NSAlert")?, new];
        if alert.is_null() {
            return None;
        }
        let _: () = msg_send![alert, setMessageText: &*NSString::from_str(title)];
        let _: () = msg_send![alert, setInformativeText: &*NSString::from_str(info)];
        // 원문은 고정폭 읽기 전용 칸에 — 길면 칸 안에서 스크롤한다(시트가 화면 밖으로 자라지 않게).
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 260.0));
        let scroll: *mut AnyObject = msg_send![AnyClass::get(c"NSScrollView")?, alloc];
        let scroll: *mut AnyObject = msg_send![scroll, initWithFrame: frame];
        let _: () = msg_send![scroll, setHasVerticalScroller: Bool::YES];
        let _: () = msg_send![scroll, setBorderType: 2isize];
        let text: *mut AnyObject = msg_send![AnyClass::get(c"NSTextView")?, alloc];
        let text: *mut AnyObject = msg_send![text, initWithFrame: frame];
        let _: () = msg_send![text, setString: &*NSString::from_str(body)];
        let _: () = msg_send![text, setEditable: Bool::NO];
        let _: () = msg_send![text, setSelectable: Bool::YES];
        let _: () = msg_send![text, setVerticallyResizable: Bool::YES];
        let _: () = msg_send![text, setAutoresizingMask: 2usize];
        let font: *mut AnyObject = msg_send![AnyClass::get(c"NSFont")?, monospacedSystemFontOfSize: 12.0f64, weight: 0.0f64];
        if !font.is_null() {
            let _: () = msg_send![text, setFont: font];
        }
        let _: () = msg_send![scroll, setDocumentView: text];
        let _: () = msg_send![text, release];
        let _: () = msg_send![alert, setAccessoryView: scroll];
        let _: () = msg_send![scroll, release];
        let allow: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("허락")];
        let deny: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*NSString::from_str("거절")];
        // Return 은 거절이다 — 터미널에서 치던 Enter 가 시트로 새도 허락이 나가지 않게.
        let _: () = msg_send![allow, setKeyEquivalent: &*NSString::from_str("")];
        let _: () = msg_send![deny, setKeyEquivalent: &*NSString::from_str("\r")];
        let _: () = msg_send![allow, setHasDestructiveAction: Bool::YES];
        if !allow_enabled {
            let _: () = msg_send![allow, setEnabled: Bool::NO];
        }
        let alert_addr = alert as usize;
        let handler = block2::RcBlock::new(move |resp: isize| {
            // NSAlertFirstButtonReturn = 1000(허락), 1001(거절). 그 밖(거둠)은 결정 없음.
            answer(match resp {
                1000 => Some(true),
                1001 => Some(false),
                _ => None,
            });
            let alert = alert_addr as *mut AnyObject;
            let _: () = msg_send![alert, release];
        });
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![ns_window, makeKeyAndOrderFront: nil];
        let _: () = msg_send![alert, beginSheetModalForWindow: ns_window, completionHandler: &*handler];
        Some(alert_addr)
    }
}

/// 검증 실행이 시트 단추를 누른다 — 0 허락, 1 거절.
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

/// 검증 실행이 시트 창을 PNG 로 — 화면 기록 권한 없이 그 창의 뷰를 비트맵으로 그린다.
#[cfg(target_os = "macos")]
fn shot_sheet(alert: usize, path: &str) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSRect, NSString};
    unsafe {
        let alert = alert as *mut AnyObject;
        let window: *mut AnyObject = msg_send![alert, window];
        if window.is_null() {
            return;
        }
        let view: *mut AnyObject = msg_send![window, contentView];
        let view: *mut AnyObject = msg_send![view, superview];
        if view.is_null() {
            return;
        }
        let bounds: NSRect = msg_send![view, bounds];
        let rep: *mut AnyObject = msg_send![view, bitmapImageRepForCachingDisplayInRect: bounds];
        if rep.is_null() {
            return;
        }
        let _: () = msg_send![view, cacheDisplayInRect: bounds, toBitmapImageRep: rep];
        let Some(dict) = AnyClass::get(c"NSDictionary") else { return };
        let props: *mut AnyObject = msg_send![dict, dictionary];
        // NSBitmapImageFileTypePNG = 4.
        let data: *mut AnyObject = msg_send![rep, representationUsingType: 4usize, properties: props];
        if !data.is_null() {
            let _: Bool = msg_send![data, writeToFile: &*NSString::from_str(path), atomically: Bool::YES];
        }
    }
}

#[cfg(windows)]
fn shot_sheet(alert: usize, path: &str) {
    crate::win_sheet::shot(alert, path);
}

#[cfg(not(any(target_os = "macos", windows)))]
fn shot_sheet(_alert: usize, _path: &str) {}

/// 다른 곳에서 닫힌 요청의 시트를 거둔다(결정 없음으로 끝난다).
#[cfg(target_os = "macos")]
fn close_sheet(alert: usize) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    unsafe {
        let alert = alert as *mut AnyObject;
        let sheet: *mut AnyObject = msg_send![alert, window];
        if sheet.is_null() {
            return;
        }
        let parent: *mut AnyObject = msg_send![sheet, sheetParent];
        if !parent.is_null() {
            let _: () = msg_send![parent, endSheet: sheet, returnCode: 0isize];
        }
    }
}

/// 맥 시트와 같은 글·단추 — 원문은 고정폭 읽기 전용 칸에서 스크롤하고, Enter 는 거절, 잘린 요청은 허락이 꺼진다.
#[cfg(windows)]
fn open_sheet(
    window: &winit::window::Window,
    title: &str,
    info: &str,
    body: &str,
    allow_enabled: bool,
    answer: impl Fn(Option<bool>) + 'static,
) -> Option<usize> {
    let spec = crate::win_sheet::Spec {
        title,
        info,
        body: Some(body),
        buttons: ["허락", "거절"],
        primary_enabled: allow_enabled,
    };
    crate::win_sheet::open(window, &spec, move |choice| answer(choice.map(|i| i == 0)))
}

#[cfg(windows)]
fn close_sheet(alert: usize) {
    crate::win_sheet::close(alert);
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_sheet(
    _window: &winit::window::Window,
    _title: &str,
    _info: &str,
    _body: &str,
    _allow_enabled: bool,
    _answer: impl Fn(Option<bool>) + 'static,
) -> Option<usize> {
    None
}

#[cfg(not(any(target_os = "macos", windows)))]
fn close_sheet(_alert: usize) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval() -> serde_json::Value {
        serde_json::json!({
            "id": "apv_1", "state": "pending", "machine": "맥미니", "device": "dev_mini", "student": "유우카",
            "pane": "%3", "cwd": "/repo", "tool": "Bash", "truncated": false,
            "fields": [{"name":"command","label":"명령","text":"rm -rf build\necho done"},{"name":"description","label":"설명","text":"정리"}],
        })
    }

    #[test]
    fn notification_click_cannot_reopen_a_resolved_or_secret_request() {
        let mut a = serde_json::json!({"state":"pending", "kind":"permission", "device":"remote"});
        assert!(notification_can_open(&a, Some("local")));
        assert!(!notification_can_open(&a, Some("remote")));
        a["state"] = "allowed".into();
        assert!(!notification_can_open(&a, Some("local")));
        a["state"] = "pending".into(); a["kind"] = "secret".into();
        assert!(!notification_can_open(&a, Some("local")));
    }

    #[test]
    fn the_notice_names_who_where_and_the_first_line_and_the_sheet_shows_everything() {
        let a = approval();
        assert_eq!(notice_text(&a), "유우카 승인 요청 · 맥미니 · Bash · rm -rf build");
        let body = sheet_body(&a);
        assert!(body.contains("[폴더]\n/repo"));
        assert!(body.contains("[명령]\nrm -rf build\necho done"));
        assert!(body.contains("[설명]\n정리"));
        let mut cut = a.clone();
        cut["truncated"] = serde_json::json!(true);
        assert!(sheet_body(&cut).contains("허락은 원래 창에서만"));
    }

    #[test]
    fn a_closed_request_says_where_it_closed() {
        let mut a = approval();
        a["state"] = serde_json::json!("allowed");
        a["by"] = serde_json::json!({"label": "아이폰"});
        assert_eq!(closed_line(&a), "유우카 요청 · 아이폰에서 허락했어요");
        a["state"] = serde_json::json!("expired");
        assert_eq!(closed_line(&a), "유우카 요청 · 2분이 지나 원래 창으로 돌아갔어요");
    }
}
