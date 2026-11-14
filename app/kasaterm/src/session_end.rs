//! Windows 세션 끝(로그오프·끄기·다시 시작)을 앱이 알아듣는 곳.
//!
//! 끝을 알리면 두 가지를 한다. 주기 폴러가 자식을 그만 띄우게 `kasa_socket::session_end` 플래그를
//! 세우고(왜 띄우면 안 되는지는 그 모듈 머리말), 칸이 아직 살아 있는 지금 작업 공간·창 틀을 저장한다.
//! 끝 단계에서는 칸 프로세스가 먼저 죽어 칸이 걷히므로, 그 뒤의 자동 저장은 무너진 배치로 복원
//! 스냅샷을 덮는다 — 그래서 끝나는 동안 다른 저장은 막는다(`autosave_session` 등).
//!
//! winit 0.30 은 WM_QUERYENDSESSION/WM_ENDSESSION 을 앱에 넘기지 않고, `with_msg_hook` 은 보내진
//! (sent) 메시지를 못 본다. 그래서 제 스레드에 숨은 최상위 창을 하나 따로 띄워 받는다 — 메시지 전용
//! 창(HWND_MESSAGE)에는 이 알림이 오지 않는다.

use super::*;

impl App {
    /// 세션이 끝나 간다 — 칸이 살아 있을 때의 복원 스냅샷과 창 틀을 남긴다. 플래그는 숨은 창이 이미
    /// 세웠으므로 여기 저장은 플래그를 보지 않는 함수를 바로 부른다.
    pub(crate) fn on_session_ending(&self, done: &std::sync::mpsc::Sender<()>) {
        self.save_session_state();
        self.save_window_frame();
        let _ = done.send(());
    }
}

/// 숨은 창 스레드를 띄운다. 실패하면 조용히 없이 간다 — 오류 모드(`quiet_loader_error_boxes`)가
/// 남아 있어 끌 때 창은 어차피 안 뜬다.
#[cfg(windows)]
pub(crate) fn watch(proxy: EventLoopProxy<UserEvent>) {
    let _ = std::thread::Builder::new().name("session-end".into()).spawn(move || win::run(proxy));
}

#[cfg(windows)]
mod win {
    use super::UserEvent;
    use std::cell::RefCell;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, ENDSESSION_CLOSEAPP, MSG,
        WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
    };
    use winit::event_loop::EventLoopProxy;

    /// 저장을 기다리는 한도. WM_QUERYENDSESSION 에 5초쯤 답이 없으면 Windows 가 「이 앱이 종료를
    /// 막고 있다」 화면에 올린다 — 그보다 짧게 끊는다.
    const SAVE_WAIT: Duration = Duration::from_secs(3);

    thread_local! {
        static PROXY: RefCell<Option<EventLoopProxy<UserEvent>>> = const { RefCell::new(None) };
    }

    pub(super) fn run(proxy: EventLoopProxy<UserEvent>) {
        PROXY.with(|p| *p.borrow_mut() = Some(proxy));
        let class: Vec<u16> = "kasaterm-session-end\0".encode_utf16().collect();
        // SAFETY: 이 스레드가 만든 창 하나와 그 메시지 루프. 문자열은 널 끝이고 함수 끝까지 산다.
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&wc) == 0 {
                return;
            }
            // 보이지 않는 최상위 창 — 최상위여야 끝 알림을 받는다.
            let hwnd = CreateWindowExW(
                0, class.as_ptr(), class.as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0,
                std::ptr::null_mut(), std::ptr::null_mut(), instance, std::ptr::null(),
            );
            if hwnd.is_null() {
                return;
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_QUERYENDSESSION => {
                // 재시작 관리자(설치 프로그램)가 이 앱만 닫으려는 것이면 세션 데스크톱은 그대로라
                // 폴러를 멈출 까닭이 없다. 저장만 한다.
                if lparam as u32 & ENDSESSION_CLOSEAPP == 0 {
                    kasa_socket::session_end::set_ending(true);
                }
                save_and_wait();
                1
            }
            WM_ENDSESSION => {
                if wparam == 0 {
                    kasa_socket::session_end::set_ending(false);
                }
                0
            }
            // SAFETY: 받은 인자를 그대로 넘긴다.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn save_and_wait() {
        let (tx, rx) = std::sync::mpsc::channel();
        let sent = PROXY.with(|p| {
            p.borrow().as_ref().is_some_and(|p| p.send_event(UserEvent::SessionEnding(tx)).is_ok())
        });
        if sent {
            let _ = rx.recv_timeout(SAVE_WAIT);
        }
    }
}
