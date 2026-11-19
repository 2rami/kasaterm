//! 트레이(맥 메뉴 막대·Windows 알림 영역) — 창의 닫기 단추는 앱을 숨기기만 하고, 여기 「종료」로만 끈다. 학생 세션이
//! 창을 닫았다고 같이 꺼지면 안 된다(2026-11-19 「x하면 트레이로 가게, 거기서 종료해야 꺼지게, 클로드앱처럼」).
//! 맥은 숨기기가 ⌘H 와 같아(앱 숨김) Dock 아이콘·⌘Tab 으로도 돌아온다. ⌘Q 는 그대로 끈다.

use muda::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::{App, UserEvent};

pub(crate) struct Tray {
    _icon: TrayIcon,
    pub open: MenuItem,
    pub quit: MenuItem,
}

/// 트레이를 세운다. 격리 검증 실행·lite 는 사람의 메뉴 막대를 어지럽히지 않게 세우지 않고 예전처럼 닫기가 끈다.
pub(crate) fn create(proxy: &winit::event_loop::EventLoopProxy<UserEvent>) -> Option<Tray> {
    if crate::verification_run() && std::env::var_os("KASATERM_TEST_TRAY").is_none() {
        return None;
    }
    let open = MenuItem::new("카사텀 열기", true, None);
    let quit = MenuItem::new("종료", true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit]).ok()?;
    let icon = tray_icon_image()?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .with_tooltip("kasaterm")
        // 맥 메뉴 막대 아이콘은 글자색을 따르는 본뜨기 그림이라야 밝은·어두운 막대에서 다 보인다.
        .with_icon_as_template(cfg!(target_os = "macos"))
        // Windows 는 왼쪽 누름이 창을 열고 오른쪽 누름이 메뉴다(알림 영역 관례). 맥은 누르면 메뉴.
        .with_menu_on_left_click(cfg!(target_os = "macos"))
        .build()
        .map_err(|e| eprintln!("[tray] 트레이를 못 세웠다: {e}"))
        .ok()?;
    let proxy = proxy.clone();
    tray_icon::TrayIconEvent::set_event_handler(Some(move |event: tray_icon::TrayIconEvent| {
        if let tray_icon::TrayIconEvent::Click { button: tray_icon::MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. } = event {
            let _ = proxy.send_event(UserEvent::ShowMainWindow);
        }
    }));
    Some(Tray { _icon: tray, open, quit })
}

/// 우산 표(하단바·설치 화면과 같은 Lucide 우산). 맥은 검정 본뜨기, Windows 는 강조색 판 위 흰 우산(앱 표와 같은 결).
fn tray_icon_image() -> Option<Icon> {
    const UMBRELLA: &str = include_str!("../assets/icons/umbrella.svg");
    let (svg, px) = if cfg!(target_os = "macos") {
        (UMBRELLA.replace("currentColor", "#000000").replace("stroke-width=\"2\"", "stroke-width=\"2.2\""), 36)
    } else {
        let inner = UMBRELLA
            .lines()
            .filter(|l| l.trim_start().starts_with("<path"))
            .collect::<Vec<_>>()
            .join("");
        (
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\"><rect width=\"24\" height=\"24\" rx=\"6\" fill=\"#2F6FD4\"/>\
                 <g transform=\"translate(3.6 3.6) scale(0.7)\" fill=\"none\" stroke=\"#ffffff\" stroke-width=\"2.4\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{inner}</g></svg>"
            ),
            32,
        )
    };
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px, px)?;
    let size = tree.size();
    let scale = px as f32 / size.width().max(size.height());
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    // tiny_skia 는 미리 곱한 알파로 내고 트레이는 곧은 알파를 받는다 — 안 되돌리면 가장자리가 어둡게 번진다.
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Icon::from_rgba(rgba, px, px).ok()
}

/// 이 창이 트레이로 숨는 본창이라는 표 — 바로가기로 다시 실행된 프로세스가 숨은 창을 찾는다(`hand_off`).
#[cfg(windows)]
pub(crate) fn mark_main(window: &winit::window::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HANDLE, HWND};
    use windows::Win32::UI::WindowsAndMessaging::SetPropW;
    let Ok(handle) = window.window_handle() else { return };
    if let RawWindowHandle::Win32(h) = handle.as_raw() {
        let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
        let _ = unsafe { SetPropW(hwnd, MAIN_MARK, Some(HANDLE(1 as *mut core::ffi::c_void))) };
    }
}

#[cfg(windows)]
const MAIN_MARK: windows::core::PCWSTR = windows::core::w!("kasaterm.main");

/// 다른 프로세스의 숨은 본창을 찾아 보이고 앞으로 올린다. 찾았으면 참 — 이 실행은 아무것도 쓰지 않고 물러난다. Windows 는
/// 바로가기를 다시 누를 때마다 새 프로세스가 떠, 그대로 두면 숨긴 창과 같은 세션을 쓰는 앱이 둘이 된다.
#[cfg(windows)]
pub(crate) fn hand_off() -> bool {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetPropW, GetWindowThreadProcessId, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        let found = unsafe { &mut *(data.0 as *mut Option<HWND>) };
        if !unsafe { GetPropW(hwnd, MAIN_MARK) }.0.is_null() {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != std::process::id() {
                *found = Some(hwnd);
                return false.into();
            }
        }
        true.into()
    }
    let mut found: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Option<HWND> as isize));
    }
    let Some(hwnd) = found else { return false };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

impl App {
    /// 닫기 단추 — 트레이가 있으면 앱을 숨기고 돌린다. 없으면(검증 실행·lite) 예전처럼 끄는 길로 간다.
    pub(crate) fn close_main_window(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.tray.is_none() {
            self.quit_app(event_loop);
            return;
        }
        #[cfg(target_os = "macos")]
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            objc2_app_kit::NSApplication::sharedApplication(mtm).hide(None);
        }
        #[cfg(not(target_os = "macos"))]
        if let Some(w) = self.window.as_ref() {
            w.set_visible(false);
        }
    }

    /// 트레이 「카사텀 열기」·트레이 누름·알림 누름 — 숨긴 창을 다시 앞으로.
    pub(crate) fn show_main_window(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            app.unhide(None);
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
        }
        if let Some(w) = self.window.as_ref() {
            w.set_visible(true);
            w.set_minimized(false);
            w.focus_window();
        }
        self.chrome_dirty = true;
    }

    /// 앱을 끈다 — 열린 문서를 먼저 저장하게 하고, 도는 일이 있으면 창 안에서 묻는다(그래서 창을 먼저 보인다).
    pub(crate) fn quit_app(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        if self.tray.is_some() {
            self.show_main_window();
        }
        if self.aux_flush_for_app_close() {
            return;
        }
        if !self.confirm_or_close_window() {
            event_loop.exit();
        }
    }
}
