//! macOS `.md` 더블클릭(Apple Event `kAEOpenDocuments` = 'odoc') 핸들러.
//!
//! winit 0.30 의 NSApplicationDelegate 는 `application:openURLs:`/`openFile:` 를
//! 구현하지 않고 winit `Event` 에도 파일오픈이 없다. 그래서 Finder 더블클릭/
//! `open(1)` 이 보내는 'odoc' Apple Event 를 받으려면 NSAppleEventManager 에
//! 핸들러를 직접 건다. winit 은 odoc 를 안 들어 경합이 없다. 추출한 경로는
//! `EventLoopProxy` 로 GUI 스레드(`UserEvent::OpenMarkdownWindow`)에 위임한다.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

// FourCharCode(= u32). 'aevt' kCoreEventClass, 'odoc' kAEOpenDocuments,
// '----' keyDirectObject(직접객체 파라미터 키).
const K_CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const K_AE_OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
const KEY_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

struct Ivars {
    proxy: EventLoopProxy<UserEvent>,
}

define_class!(
    // SAFETY:
    // - 상위 NSObject 는 서브클래싱 제약이 없다.
    // - OpenDocHandler 는 Drop 을 구현하지 않는다.
    #[unsafe(super(NSObject))]
    #[name = "KasatermOpenDocHandler"]
    #[ivars = Ivars]
    struct OpenDocHandler;

    unsafe impl NSObjectProtocol for OpenDocHandler {}

    impl OpenDocHandler {
        // NSAppleEventManager 규약 셀렉터. odoc 의 직접객체(keyDirectObject)는
        // 파일 리스트(AEList) — 각 항목을 파일 URL→경로로 풀어 GUI 에 위임한다.
        #[unsafe(method(handleAppleEvent:withReplyEvent:))]
        fn handle_apple_event(
            &self,
            event: &NSAppleEventDescriptor,
            _reply: &NSAppleEventDescriptor,
        ) {
            let Some(list) = event.paramDescriptorForKeyword(KEY_DIRECT_OBJECT) else {
                return;
            };
            let n = list.numberOfItems();
            // AE 인덱스는 1-base. 단일 파일이면 n == 1.
            for i in 1..=n {
                let Some(item) = list.descriptorAtIndex(i) else {
                    continue;
                };
                // fileURLValue().path() 가 디코딩된 경로(한글/공백 안전). 실패 시
                // stringValue 폴백(percent-encoding 잔존 가능 — 차선).
                let path = item
                    .fileURLValue()
                    .and_then(|url| url.path())
                    .or_else(|| item.stringValue())
                    .map(|s| s.to_string());
                if let Some(p) = path {
                    let _ = self
                        .ivars()
                        .proxy
                        .send_event(UserEvent::OpenMarkdownWindow(p));
                }
            }
        }
    }
);

impl OpenDocHandler {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { proxy });
        unsafe { msg_send![super(this), init] }
    }
}

/// odoc Apple Event 핸들러를 1회 등록한다. `Once` 가드라 main() 1차 등록과
/// resumed() 2차 등록이 중복되지 않는다. 핸들러 객체는 이벤트가 올 때마다
/// 불리므로 NSApp 수명 동안 살아야 한다 → leak(`mem::forget`).
pub(crate) fn install_open_doc_handler(proxy: EventLoopProxy<UserEvent>) {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let handler = OpenDocHandler::new(proxy);
        let mgr = NSAppleEventManager::sharedAppleEventManager();
        unsafe {
            mgr.setEventHandler_andSelector_forEventClass_andEventID(
                &handler,
                sel!(handleAppleEvent:withReplyEvent:),
                K_CORE_EVENT_CLASS,
                K_AE_OPEN_DOCUMENTS,
            );
        }
        std::mem::forget(handler);
    });
}

/// 표준 edit 액션(`copy:`/`paste:`)을 `NSApplication.sendAction:to:from:` 으로
/// key 창의 first responder 에 보낸다. to=nil 이면 responder chain 을 타는 표준
/// 동작 — 아로나 webview 가 key 면 거기서 처리(true 반환)하고, 터미널 창처럼
/// `paste:` 를 구현한 responder 가 없으면 false 를 반환한다.
///
/// **왜:** 네이티브 Edit 메뉴(Cmd+V/Cmd+C)가 단축키 keyDown 을 가로채 winit 까지
/// 안 내려와 터미널 paste/copy 가 먹통이었다. 메뉴 항목을 커스텀으로 돌려
/// MenuEvent 로 받은 뒤, 먼저 이 함수로 webview 우선 위임하고 false 면 호출측이
/// 직접 클립보드를 처리한다.
#[cfg(target_os = "macos")]
fn send_app_edit_action(action: objc2::runtime::Sel) -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyObject, Bool};
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSApplication") else {
            return false;
        };
        let app: *mut AnyObject = msg_send![cls, sharedApplication];
        if app.is_null() {
            return false;
        }
        let nil: *mut AnyObject = std::ptr::null_mut();
        let handled: Bool = msg_send![app, sendAction: action, to: nil, from: nil];
        handled.as_bool()
    }
}

/// Cmd+V 메뉴 → key 창 first responder 에 `paste:` 위임. webview 가 받으면 true.
#[cfg(target_os = "macos")]
pub(crate) fn send_paste_action() -> bool {
    send_app_edit_action(objc2::sel!(paste:))
}

/// Cmd+C 메뉴 → key 창 first responder 에 `copy:` 위임. webview 가 받으면 true.
#[cfg(target_os = "macos")]
pub(crate) fn send_copy_action() -> bool {
    send_app_edit_action(objc2::sel!(copy:))
}

/// ⌘Q 종료 확인 — ghostty 식 NSAlert("종료"/"취소"). "종료"(첫 버튼)면 true.
/// PredefinedMenuItem::quit(OS 가 곧장 terminate) 대신 커스텀 ⌘Q 메뉴가 이걸 띄워,
/// 확인 시에만 호출측이 event_loop.exit()로 정상 종료(세션·window.json 저장)한다.
/// alert 를 못 띄우면(클래스 없음 등) 막지 않고 true 를 돌려 종료를 진행한다.
#[cfg(target_os = "macos")]
pub(crate) fn confirm_quit() -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSAlert") else {
            return true;
        };
        let alert: *mut AnyObject = msg_send![cls, new];
        if alert.is_null() {
            return true;
        }
        let title = NSString::from_str("kasaterm 을 종료할까요?");
        let info = NSString::from_str("실행 중인 모든 터미널 세션이 종료됩니다.");
        let quit_btn = NSString::from_str("종료");
        let cancel_btn = NSString::from_str("취소");
        let _: () = msg_send![alert, setMessageText: &*title];
        let _: () = msg_send![alert, setInformativeText: &*info];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*quit_btn];
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: &*cancel_btn];
        // NSAlertFirstButtonReturn = 1000 ("종료"), Second = 1001 ("취소").
        let resp: isize = msg_send![alert, runModal];
        resp == 1000
    }
}
