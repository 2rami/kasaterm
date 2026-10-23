//! Windows 스포이드 — 맥 `NSColorSampler` 와 같은 꼴(커서를 따라가는 둥근 확대경, 누르면 그 색을
//! 집고 Esc 면 취소)을 GDI 로 짓는다. Windows 는 화면 픽셀 읽기에 권한이 필요 없다.
//!
//! 화면 전체를 덮는 거의 투명한(알파 1) 창이 누름·키를 받고, 확대경은 그 위에서 누름이 지나가는
//! 창으로 커서를 따라간다. 두 창 다 화면 캡처에서 빼므로(`WDA_EXCLUDEFROMCAPTURE`) 우리가 읽는
//! 화면 픽셀에 우리 창이 섞이지 않는다 — 그래서 확대경을 커서 한가운데에 둘 수 있다.
//!
//! 덮개 창은 남의 누름을 먹는다. 그래서 끝나는 길을 여럿 둔다: 왼쪽 누름(집기), Esc·오른쪽 누름·
//! 다른 창으로 초점이 넘어감(취소), 그리고 [`LIMIT`] 이 지나면 저절로 취소.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, MonitorFromPoint,
    ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, MONITOR_DEFAULTTONEAREST, SRCCOPY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, KillTimer, LoadCursorW, PostQuitMessage, RegisterClassW, SetCursor,
    SetForegroundWindow, SetLayeredWindowAttributes, SetTimer, SetWindowDisplayAffinity, ShowWindow,
    TranslateMessage, UpdateLayeredWindow, HTTRANSPARENT, IDC_CROSS, LWA_ALPHA, MSG, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOWNOACTIVATE, SW_SHOW, ULW_ALPHA,
    WA_INACTIVE, WDA_EXCLUDEFROMCAPTURE, WM_ACTIVATE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MOUSEMOVE, WM_NCHITTEST, WM_RBUTTONDOWN, WM_SETCURSOR, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

/// 확대경에 담는 화면 픽셀 수(한 변). 홀수라야 가운데 픽셀이 하나다.
pub(super) const SPAN: usize = 13;
/// 화면 픽셀 하나를 확대경에서 몇 논리 px 로 그리나. 13 × 10 = 지름 130 — 맥 확대경과 비슷한 크기.
const CELL: f32 = 10.0;
/// 덮개가 남의 누름을 먹는 시간의 상한.
const LIMIT: Duration = Duration::from_secs(60);
const TICK: u32 = 16;

/// 두 번 눌러 덮개가 둘 뜨면 하나가 끝나도 다른 하나가 화면을 막고 남는다.
static RUNNING: AtomicBool = AtomicBool::new(false);

struct Session {
    slot: crate::SettingsInput,
    overlay: HWND,
    loupe: HWND,
    origin: POINT,
    screen: HDC,
    sample: Dib,
    lens: Option<Dib>,
    /// 우리 창이 캡처에서 빠졌나. 못 뺐으면 확대경을 커서에서 비켜 둬야 제 그림을 다시 읽지 않는다.
    hidden: bool,
    started: Instant,
    done: bool,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// 위에서 아래로 놓인 32bpp DIB. `bits` 는 그 DC 에 고른 비트맵의 픽셀(BGRA, u32 하나가 한 픽셀).
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    bits: *mut u32,
    side: usize,
}

impl Dib {
    unsafe fn new(screen: HDC, side: usize) -> Option<Dib> {
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: side as i32,
            // 음수 높이 = 위에서 아래로. 그래야 픽셀 순서가 화면 순서와 같다.
            biHeight: -(side as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        unsafe {
            let bitmap = CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let dc = CreateCompatibleDC(Some(screen));
            SelectObject(dc, bitmap.into());
            Some(Dib { dc, bitmap, bits: bits.cast(), side })
        }
    }

    fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, self.side * self.side) }
    }

    fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.side * self.side) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bitmap.into());
        }
    }
}

/// 스포이드를 띄운다. 창은 자기 스레드의 메시지 루프에서 돈다 — 끝나면 GUI 를 깨워
/// `pump_eyedropper` 가 집힌 색을 꺼내게 한다.
pub(super) fn start(slot: crate::SettingsInput, proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        unsafe { run(slot) };
        RUNNING.store(false, Ordering::SeqCst);
        let _ = proxy.send_event(crate::UserEvent::Redraw);
    });
}

unsafe fn run(slot: crate::SettingsInput) {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance = module.into();
        // 두 번째 세션부터는 이미 있다고 실패하지만 같은 클래스라 그대로 쓴다.
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(overlay_proc), hInstance: instance, lpszClassName: w!("KasatermEyedropper"), ..Default::default() });
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(loupe_proc), hInstance: instance, lpszClassName: w!("KasatermEyedropperLoupe"), ..Default::default() });
        let origin = POINT { x: GetSystemMetrics(SM_XVIRTUALSCREEN), y: GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let (width, height) = (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN));
        let Ok(overlay) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED, w!("KasatermEyedropper"), w!(""), WS_POPUP,
            origin.x, origin.y, width, height, None, None, Some(instance), None,
        ) else { return };
        let Ok(loupe) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
            w!("KasatermEyedropperLoupe"), w!(""), WS_POPUP, 0, 0, 1, 1, Some(overlay), None, Some(instance), None,
        ) else {
            let _ = DestroyWindow(overlay);
            return;
        };
        // 알파 0 인 창은 누름을 못 받는다. 1 이면 눈에 안 보이면서 누름·키를 받는다.
        let _ = SetLayeredWindowAttributes(overlay, COLORREF(0), 1, LWA_ALPHA);
        let hidden = SetWindowDisplayAffinity(overlay, WDA_EXCLUDEFROMCAPTURE).is_ok()
            & SetWindowDisplayAffinity(loupe, WDA_EXCLUDEFROMCAPTURE).is_ok();
        let screen = GetDC(None);
        let Some(sample) = Dib::new(screen, SPAN) else {
            ReleaseDC(None, screen);
            let _ = DestroyWindow(overlay);
            return;
        };
        SESSION.with(|cell| {
            *cell.borrow_mut() = Some(Session { slot, overlay, loupe, origin, screen, sample, lens: None, hidden, started: Instant::now(), done: false });
        });
        let _ = ShowWindow(overlay, SW_SHOW);
        let _ = SetForegroundWindow(overlay);
        let _ = SetFocus(Some(overlay));
        SetTimer(Some(overlay), 1, TICK, None);
        refresh();
        let _ = ShowWindow(loupe, SW_SHOWNOACTIVATE);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if let Some(session) = SESSION.with(|cell| cell.borrow_mut().take()) {
            drop(session.lens);
            drop(session.sample);
            ReleaseDC(None, session.screen);
        }
    }
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_SETCURSOR => {
                // 확대경이 커서 자리에 서면 그 가운데 칸이 곧 조준점이다 — 커서를 겹쳐 그리면 고를 픽셀을 가린다.
                let centered = SESSION.with(|cell| cell.borrow().as_ref().is_some_and(|session| session.hidden));
                SetCursor(if centered { None } else { LoadCursorW(None, IDC_CROSS).ok() });
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                refresh();
                LRESULT(0)
            }
            WM_TIMER => {
                let expired = SESSION.with(|cell| cell.borrow().as_ref().is_some_and(|session| session.started.elapsed() >= LIMIT));
                if expired { finish(None) } else { refresh() }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                // 누른 자리는 메시지가 실어 온다(덮개 안 좌표). 커서를 다시 물으면 그 사이 움직인 자리를 집는다.
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                let picked = SESSION.with(|cell| {
                    cell.borrow_mut().as_mut().and_then(|session| {
                        capture(session, POINT { x: session.origin.x + x, y: session.origin.y + y })
                            .then(|| center(session.sample.pixels()))
                    })
                });
                finish(picked);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                finish(None);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
                finish(None);
                LRESULT(0)
            }
            WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => {
                finish(None);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe extern "system" fn loupe_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// 한 번만 끝낸다 — 창을 부수는 동안 초점이 넘어가 WM_ACTIVATE 가 다시 이리로 온다.
unsafe fn finish(picked: Option<[u8; 3]>) {
    let target = SESSION.with(|cell| {
        let mut cell = cell.borrow_mut();
        let session = cell.as_mut()?;
        if session.done {
            return None;
        }
        session.done = true;
        Some((session.overlay, session.loupe, session.slot))
    });
    let Some((overlay, loupe, slot)) = target else { return };
    if let Some(rgb) = picked {
        super::deliver(slot, rgb);
    }
    unsafe {
        let _ = KillTimer(Some(overlay), 1);
        let _ = DestroyWindow(loupe);
        let _ = DestroyWindow(overlay);
    }
}

/// 커서 둘레를 다시 읽고 확대경을 그 자리에 다시 그린다.
unsafe fn refresh() {
    SESSION.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(session) = cell.as_mut().filter(|session| !session.done) else { return };
        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_err() || !unsafe { capture(session, cursor) } {
            return;
        }
        let scale = unsafe { monitor_scale(cursor) };
        let side = ((SPAN as f32 * CELL * scale).round() as usize).max(SPAN);
        if session.lens.as_ref().is_none_or(|lens| lens.side != side) {
            session.lens = unsafe { Dib::new(session.screen, side) };
        }
        let Some(lens) = session.lens.as_mut() else { return };
        render_lens(session.sample.pixels().try_into().unwrap(), side, scale, lens.pixels_mut());
        let half = side as i32 / 2;
        // 캡처에서 못 뺐으면 커서 오른쪽 아래로 비켜 둔다 — 겹치면 다음 표본이 확대경 자신을 읽는다.
        let at = if session.hidden {
            POINT { x: cursor.x - half, y: cursor.y - half }
        } else {
            let gap = (24.0 * scale) as i32;
            POINT { x: cursor.x + gap, y: cursor.y + gap }
        };
        let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
        let size = SIZE { cx: side as i32, cy: side as i32 };
        let _ = unsafe {
            UpdateLayeredWindow(session.loupe, None, Some(&at), Some(&size), Some(lens.dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA)
        };
    });
}

unsafe fn capture(session: &mut Session, at: POINT) -> bool {
    unsafe { grab(session.screen, &session.sample, at) }
}

/// `at` 을 가운데로 SPAN × SPAN 화면 픽셀을 표본 DIB 에 옮긴다. CAPTUREBLT 라야 다른 앱의
/// 겹친(layered) 창도 화면에 보이는 그대로 담긴다.
unsafe fn grab(screen: HDC, sample: &Dib, at: POINT) -> bool {
    let half = SPAN as i32 / 2;
    unsafe { BitBlt(sample.dc, 0, 0, SPAN as i32, SPAN as i32, Some(screen), at.x - half, at.y - half, SRCCOPY | CAPTUREBLT) }.is_ok()
}

unsafe fn monitor_scale(at: POINT) -> f32 {
    let monitor = unsafe { MonitorFromPoint(at, MONITOR_DEFAULTTONEAREST) };
    let (mut x, mut y) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_err() {
        return 1.0;
    }
    x as f32 / 96.0
}

/// 표본 가운데 픽셀(DIB 의 0x00RRGGBB) → sRGB. GDI 가 읽는 화면은 sRGB 라 그대로가 팔레트 hex 다.
pub(super) fn center(sample: &[u32]) -> [u8; 3] {
    let pixel = sample[(SPAN / 2) * SPAN + SPAN / 2];
    [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]
}

/// 둥근 확대경 한 장(미리 곱한 BGRA). 표본 픽셀을 칸으로 키우고 칸 사이에 옅은 격자,
/// 가운데 칸에 흰 테(바깥 검은 선), 둘레에 흰 테를 두른다. 바깥은 투명.
pub(super) fn render_lens(sample: &[u32; SPAN * SPAN], side: usize, scale: f32, out: &mut [u32]) {
    let radius = side as f32 / 2.0;
    let ring = 2.0 * scale;
    let cell = side as f32 / SPAN as f32;
    let mid = (SPAN / 2) as f32;
    let (lo, hi) = (mid * cell, (mid + 1.0) * cell);
    let stroke = (1.5 * scale).max(1.0);
    for y in 0..side {
        for x in 0..side {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let distance = ((fx - radius).powi(2) + (fy - radius).powi(2)).sqrt();
            // 가장자리 한 px 에 걸친 만큼만 덮어 계단을 지운다.
            let coverage = (radius - distance).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                out[y * side + x] = 0;
                continue;
            }
            let rgb = if distance >= radius - 1.0 {
                [0x20, 0x20, 0x20]
            } else if distance >= radius - 1.0 - ring {
                [0xff, 0xff, 0xff]
            } else {
                let (sx, sy) = (((fx / cell) as usize).min(SPAN - 1), ((fy / cell) as usize).min(SPAN - 1));
                let pixel = sample[sy * SPAN + sx];
                let base = [(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8];
                let inside = |v: f32, a: f32, b: f32| v >= a && v < b;
                let near_center = inside(fx, lo - 1.0, hi + 1.0) && inside(fy, lo - 1.0, hi + 1.0);
                let on_white = near_center && !(inside(fx, lo + stroke, hi - stroke) && inside(fy, lo + stroke, hi - stroke));
                let on_dark = near_center && !on_white && !(inside(fx, lo, hi) && inside(fy, lo, hi));
                if on_white {
                    [0xff, 0xff, 0xff]
                } else if on_dark {
                    [0x00, 0x00, 0x00]
                } else if fx % cell < 1.0 || fy % cell < 1.0 {
                    // 격자는 칸 색을 회색 쪽으로 조금만 당긴다 — 어두운 칸에도 밝은 칸에도 보인다.
                    base.map(|c| ((c as f32) * 0.8 + 128.0 * 0.2) as u8)
                } else {
                    base
                }
            };
            let alpha = (coverage * 255.0).round() as u32;
            let premul = |c: u8| (c as f32 * coverage).round() as u32;
            out[y * side + x] = (alpha << 24) | (premul(rgb[0]) << 16) | (premul(rgb[1]) << 8) | premul(rgb[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win_dialog::hidden_desktop;
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    fn sample() -> [u32; SPAN * SPAN] {
        let mut s = [0u32; SPAN * SPAN];
        for (i, p) in s.iter_mut().enumerate() {
            let (x, y) = (i % SPAN, i / SPAN);
            *p = ((x as u32 * 19) << 16) | ((y as u32 * 19) << 8) | 0x40;
        }
        s[(SPAN / 2) * SPAN + SPAN / 2] = 0x00_3a_7b_d5;
        s
    }

    #[test]
    fn the_center_pixel_is_the_picked_color() {
        assert_eq!(center(&sample()), [0x3a, 0x7b, 0xd5]);
    }

    /// 확대경 가운데 칸이 집힐 색이고, 둘레 밖은 투명, 테는 흰색이어야 「이 픽셀을 집는다」가 보인다.
    #[test]
    fn the_lens_magnifies_the_center_and_is_round() {
        let scale = 1.5;
        let side = (SPAN as f32 * CELL * scale).round() as usize;
        let mut out = vec![0u32; side * side];
        render_lens(&sample(), side, scale, &mut out);
        let at = |x: usize, y: usize| out[y * side + x];
        assert_eq!(at(side / 2, side / 2), 0xff_3a_7b_d5, "한가운데는 집을 색 그대로");
        assert_eq!(at(0, 0) >> 24, 0, "모서리는 원 밖이라 투명");
        assert_eq!(at(side / 2, 2) & 0x00ff_ffff, 0x00ff_ffff, "위쪽 테는 흰색");
        // 사람이 보는 꼴을 그림으로 남긴다: KASATERM_TEST_LENS_PNG=<경로>.
        if let Some(path) = std::env::var_os("KASATERM_TEST_LENS_PNG") {
            let mut rgba = Vec::with_capacity(side * side * 4);
            for p in &out {
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8]);
            }
            image::save_buffer(path, &rgba, side as u32, side as u32, image::ExtendedColorType::Rgba8).unwrap();
        }
    }

    /// 덮개 흐름: 누르면 그 자리 색을 담고 끝나며, Esc 면 아무것도 안 담고 끝난다. 끝나면
    /// 스레드가 돌아와야 한다 — 안 돌아오면 덮개가 사람 화면의 누름을 계속 먹는다.
    #[test]
    #[ignore = "실제 창을 띄운다 — 대화형 데스크톱이 있는 Windows 에서 손으로 돌린다"]
    fn a_click_picks_and_escape_cancels() {
        let slot = crate::SettingsInput::PaletteHex(3);
        let _ = super::super::take_picked();
        let started = Instant::now();
        hidden_desktop::run("eyedropper-esc", move || unsafe { run(slot) }, |desk| {
            let overlay = hidden_desktop::wait_for(desk, Some("KasatermEyedropper"), None);
            unsafe { PostMessageW(Some(overlay), WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0)) }.unwrap();
        });
        assert!(started.elapsed() < Duration::from_secs(5), "Esc 뒤 곧 끝나야 한다");
        assert_eq!(super::super::take_picked(), None, "취소는 아무것도 안 담는다");

        // 숨은 데스크톱은 그려지지 않아 그 화면 DC 의 BitBlt 가 실패한다. 그럴 때 누름은 집을 색이
        // 없으니 취소로 끝나야 하고(덮개가 남으면 안 된다), 읽히는 환경이면 그 칸에 담겨야 한다.
        let readable = std::sync::Arc::new(AtomicBool::new(false));
        let seen = readable.clone();
        hidden_desktop::run("eyedropper-click", move || unsafe {
            let screen = GetDC(None);
            if let Some(dib) = Dib::new(screen, SPAN) {
                seen.store(grab(screen, &dib, POINT { x: 30, y: 40 }), Ordering::SeqCst);
            }
            ReleaseDC(None, screen);
            run(slot)
        }, |desk| {
            let overlay = hidden_desktop::wait_for(desk, Some("KasatermEyedropper"), None);
            unsafe { PostMessageW(Some(overlay), WM_LBUTTONDOWN, WPARAM(1), LPARAM((40 << 16) | 30)) }.unwrap();
        });
        let picked = super::super::take_picked();
        if readable.load(Ordering::SeqCst) {
            assert!(matches!(picked, Some((s, _)) if s == slot), "누름은 그 칸에 색을 담는다: {picked:?}");
        } else {
            assert_eq!(picked, None, "못 읽은 화면에서 지어낸 색을 담으면 안 된다");
        }
        let desk = unsafe { windows::Win32::System::StationsAndDesktops::OpenInputDesktop(Default::default(), false, windows::Win32::System::StationsAndDesktops::DESKTOP_ENUMERATE) };
        if let Ok(desk) = desk {
            assert!(hidden_desktop::find(desk, Some("KasatermEyedropper"), None).is_none(), "덮개가 사람 화면에 남으면 안 된다");
            let _ = unsafe { windows::Win32::System::StationsAndDesktops::CloseDesktop(desk) };
        }
    }

    /// 화면 픽셀을 바른 자리·바른 색 순서로 읽는가. 사람 화면 구석에 0.5초 동안 초점을 뺏지 않는
    /// 작은 색 칸을 띄우고 그 가운데를 읽는다(숨은 데스크톱은 그려지지 않아 여기선 못 잰다).
    #[test]
    #[ignore = "사람 화면에 작은 색 칸을 잠깐 띄운다 — 손으로 돌린다"]
    fn reads_the_real_screen_pixel_under_a_point() {
        use windows::Win32::Graphics::Gdi::CreateSolidBrush;
        use windows::Win32::UI::WindowsAndMessaging::{PeekMessageW, PM_REMOVE};
        unsafe {
            let instance = GetModuleHandleW(None).unwrap().into();
            // COLORREF 는 0x00BBGGRR — #3A7BD5.
            let brush = CreateSolidBrush(COLORREF(0x00d5_7b3a));
            RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(loupe_proc), hInstance: instance, hbrBackground: brush, lpszClassName: w!("KasatermEyedropperProbe"), ..Default::default() });
            let (x, y) = (GetSystemMetrics(SM_XVIRTUALSCREEN) + 8, GetSystemMetrics(SM_YVIRTUALSCREEN) + 160);
            let probe = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, w!("KasatermEyedropperProbe"), w!(""), WS_POPUP,
                x, y, 48, 48, None, None, Some(instance), None).unwrap();
            let _ = ShowWindow(probe, SW_SHOWNOACTIVATE);
            let until = Instant::now() + Duration::from_millis(500);
            let mut message = MSG::default();
            while Instant::now() < until {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let screen = GetDC(None);
            let dib = Dib::new(screen, SPAN).unwrap();
            assert!(grab(screen, &dib, POINT { x: x + 24, y: y + 24 }));
            let rgb = center(dib.pixels());
            drop(dib);
            ReleaseDC(None, screen);
            let _ = DestroyWindow(probe);
            let _ = DeleteObject(brush.into());
            assert_eq!(rgb, [0x3a, 0x7b, 0xd5]);
        }
    }
}
