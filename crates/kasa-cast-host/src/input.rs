//! 기기 화면 거울의 조작 넣기 — 보는 쪽(폰·다른 PC)이 보낸 누르기·휠·키·글자를 이 기기에 넣는다
//! (`docs/screen-mirror.md` 「조작」). 소켓·차례는 `kasa_cast`·`kasa_remote::screen_mirror`, 여기는 OS 에 넣는 일만.
//!
//! 좌표는 그 화면 안의 비율이라 화면 사각형(macOS 포인트, Windows 가상 화면 픽셀)에 얹어 푼다. 글자는 키 위치가
//! 아니라 글자째 넣는다 — 폰 자판이 이미 조합한 한글을 키로 풀면 그 기기 입력기가 다시 조합해 어긋난다.

use kasa_cast::mirror::{Denied, Input};

pub fn put(display: u32, input: &Input) -> Result<(), Denied> {
    os::put(display, input)
}

pub fn request_permission() {
    os::request_permission();
}

/// 그 기기가 이 키 이름을 누를 줄 아는가 — 모르는 키는 보는 쪽이 글자째 보낸다.
pub fn known_key(code: &str) -> bool {
    key_of(code).is_some()
}

/// Alt+클릭·Ctrl+끌기 — 처음 누를 때의 손가락을 단추를 다 뗄 때까지 쥔다. 떼는 손가락은 쥔 것 그대로다(그사이 보는 쪽에서
/// 손가락을 먼저 떼도 그 기기에 눌린 채 남지 않게). (단추보다 먼저 누를 것, 단추 뒤에 뗄 것, 이제 쥔 것) — 손가락 비트는
/// 1 shift·2 control·4 alt·8 windows.
#[cfg(any(windows, test))]
fn click_mods(prev: u8, buttons: u8, mods: u8, held: u8) -> (u8, u8, u8) {
    if buttons == 0 {
        (0, held, 0)
    } else if prev == 0 {
        (mods & !held, 0, held | mods)
    } else {
        (0, 0, held)
    }
}

/// 웹 `KeyboardEvent.code` 이름 → (macOS 가상 키, Windows VK, Windows 확장 키인가).
fn key_of(code: &str) -> Option<(u16, u16, bool)> {
    const LETTERS: [u16; 26] = [
        0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,
    ];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    const F_KEYS: [u16; 12] = [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111];
    let one = |rest: &str| {
        let mut chars = rest.chars();
        let c = chars.next()?;
        chars.next().is_none().then_some(c)
    };
    if let Some(c) = code.strip_prefix("Key").and_then(one).filter(char::is_ascii_uppercase) {
        let i = (c as u8 - b'A') as usize;
        return Some((LETTERS[i], u16::from(c as u8), false));
    }
    if let Some(c) = code.strip_prefix("Digit").and_then(one).filter(char::is_ascii_digit) {
        let i = (c as u8 - b'0') as usize;
        return Some((DIGITS[i], u16::from(c as u8), false));
    }
    if let Some(c) = code.strip_prefix("Numpad").and_then(one).filter(char::is_ascii_digit) {
        const PAD: [u16; 10] = [82, 83, 84, 85, 86, 87, 88, 89, 91, 92];
        let i = (c as u8 - b'0') as usize;
        return Some((PAD[i], 0x60 + i as u16, false));
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()).filter(|n| (1..=12).contains(n)) {
        return Some((F_KEYS[n - 1], 0x70 + n as u16 - 1, false));
    }
    Some(match code {
        "Enter" => (36, 0x0D, false),
        "NumpadEnter" => (76, 0x0D, true),
        "Tab" => (48, 0x09, false),
        "Backspace" => (51, 0x08, false),
        "Escape" => (53, 0x1B, false),
        "Space" => (49, 0x20, false),
        "Delete" => (117, 0x2E, true),
        "MetaLeft" | "MetaRight" => (55, 0x5B, true),
        "AltLeft" => (58, 0x12, false),
        "Home" => (115, 0x24, true),
        "End" => (119, 0x23, true),
        "PageUp" => (116, 0x21, true),
        "PageDown" => (121, 0x22, true),
        "ArrowLeft" => (123, 0x25, true),
        "ArrowRight" => (124, 0x27, true),
        "ArrowDown" => (125, 0x28, true),
        "ArrowUp" => (126, 0x26, true),
        "Minus" => (27, 0xBD, false),
        "Equal" => (24, 0xBB, false),
        "BracketLeft" => (33, 0xDB, false),
        "BracketRight" => (30, 0xDD, false),
        "Backslash" => (42, 0xDC, false),
        "Semicolon" => (41, 0xBA, false),
        "Quote" => (39, 0xDE, false),
        "Comma" => (43, 0xBC, false),
        "Period" => (47, 0xBE, false),
        "Slash" => (44, 0xBF, false),
        "Backquote" => (50, 0xC0, false),
        "Insert" => (114, 0x2D, true),
        "ContextMenu" => (110, 0x5D, true),
        // 맥 쪽은 F13 자리다 — 맥은 PC 키보드의 PrtSc 를 F13 으로 받는다.
        "PrintScreen" => (105, 0x2C, true),
        // 한/영·한자(웹 이름은 Lang1·Lang2). 맥 쪽은 표준 대응대로 가나·영숫 키다.
        "Lang1" => (104, 0x15, false),
        "Lang2" => (102, 0x19, false),
        "NumpadDecimal" => (65, 0x6E, false),
        "NumpadMultiply" => (67, 0x6A, false),
        "NumpadAdd" => (69, 0x6B, false),
        "NumpadSubtract" => (78, 0x6D, false),
        "NumpadDivide" => (75, 0x6F, true),
        "NumpadEqual" => (81, 0xBB, false),
        _ => return None,
    })
}

/// 두 번·세 번 누름 — 파인더·글 칸은 시각이 아니라 이벤트에 실린 누름 수를 읽는다(맥). 0.4초 안·5포인트 안이면 잇는다.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Clicks {
    at: std::time::Instant,
    x: f64,
    y: f64,
    count: i64,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Clicks {
    fn press(slot: &mut Option<Clicks>, x: f64, y: f64) -> i64 {
        let now = std::time::Instant::now();
        let count = match slot {
            Some(c) if now.duration_since(c.at).as_millis() < 400 && (c.x - x).abs() < 5.0 && (c.y - y).abs() < 5.0 => {
                (c.count + 1).min(3)
            }
            _ => 1,
        };
        *slot = Some(Clicks { at: now, x, y, count });
        count
    }
}

#[cfg(target_os = "macos")]
mod os {
    use super::{key_of, Clicks, Denied, Input};
    use std::ffi::c_void;
    use std::sync::Mutex;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Point,
    }

    type Event = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayBounds(display: u32) -> Rect;
        fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, at: Point, button: u32) -> Event;
        fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> Event;
        fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> Event;
        fn CGEventKeyboardSetUnicodeString(event: Event, len: usize, text: *const u16);
        fn CGEventSetFlags(event: Event, flags: u64);
        fn CGEventSetIntegerValueField(event: Event, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: Event);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFBooleanTrue: *const c_void;
        static kCFTypeDictionaryKeyCallBacks: u8;
        static kCFTypeDictionaryValueCallBacks: u8;
        fn CFDictionaryCreate(
            alloc: *const c_void,
            keys: *const *const c_void,
            values: *const *const c_void,
            count: isize,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        static kAXTrustedCheckOptionPrompt: *const c_void;
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    }

    const HID_TAP: u32 = 0;
    const CLICK_STATE: u32 = 1;
    const UNITS_PIXEL: u32 = 0;
    /// `CGEventCreateKeyboardEvent` 에 한 번 실을 수 있는 UTF-16 수.
    const UNICODE_CHUNK: usize = 20;

    static CLICKS: Mutex<Option<Clicks>> = Mutex::new(None);

    fn flags(mods: u8) -> u64 {
        [(1, 1 << 17), (2, 1 << 18), (4, 1 << 19), (8, 1 << 20)]
            .iter()
            .filter(|(bit, _)| mods & bit != 0)
            .fold(0, |acc, (_, mask)| acc | mask)
    }

    fn post(event: Event, mods: Option<u8>) {
        if event.is_null() {
            return;
        }
        unsafe {
            if let Some(mods) = mods {
                CGEventSetFlags(event, flags(mods));
            }
            CGEventPost(HID_TAP, event);
            CFRelease(event);
        }
    }

    fn at(display: u32, x: f64, y: f64) -> Point {
        let b = unsafe { CGDisplayBounds(display) };
        Point {
            x: b.origin.x + (x * b.size.x).min(b.size.x - 1.0).max(0.0),
            y: b.origin.y + (y * b.size.y).min(b.size.y - 1.0).max(0.0),
        }
    }

    fn mouse(kind: u32, p: Point, button: u32, mods: u8, clicks: i64) {
        let e = unsafe { CGEventCreateMouseEvent(std::ptr::null(), kind, p, button) };
        if !e.is_null() {
            unsafe { CGEventSetIntegerValueField(e, CLICK_STATE, clicks) };
        }
        post(e, Some(mods));
    }

    fn key(code: u16, down: bool, mods: u8) {
        post(unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) }, Some(mods));
    }

    pub(super) fn put(display: u32, input: &Input) -> Result<(), Denied> {
        // 권한 없이 넣으면 OS 가 소리 없이 버린다 — 넣은 줄 알고 보는 쪽이 기다리지 않게 먼저 묻는다.
        if !unsafe { AXIsProcessTrusted() } {
            return Err(Denied::Permission);
        }
        match input {
            Input::Pointer { x, y, buttons, prev, mods } => {
                let p = at(display, *x, *y);
                let changed = buttons ^ prev;
                if changed == 0 {
                    let kind = if buttons & 1 != 0 { 6 } else if buttons & 2 != 0 { 7 } else if buttons & 4 != 0 { 27 } else { 5 };
                    let button = if buttons & 2 != 0 { 1 } else if buttons & 4 != 0 { 2 } else { 0 };
                    mouse(kind, p, button, *mods, 1);
                    return Ok(());
                }
                for (mask, down, up, button) in [(1u8, 1u32, 2u32, 0u32), (2, 3, 4, 1), (4, 25, 26, 2)] {
                    if changed & mask == 0 {
                        continue;
                    }
                    let pressing = buttons & mask != 0;
                    let mut slot = CLICKS.lock().unwrap_or_else(|e| e.into_inner());
                    let clicks = if pressing { Clicks::press(&mut slot, p.x, p.y) } else { slot.as_ref().map_or(1, |c| c.count) };
                    drop(slot);
                    mouse(if pressing { down } else { up }, p, button, *mods, clicks);
                }
            }
            Input::Scroll { x, y, dx, dy } => {
                // 휠은 커서 자리의 창이 받는다 — 먼저 그 자리로 옮긴다.
                mouse(5, at(display, *x, *y), 0, 0, 1);
                post(unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null(), UNITS_PIXEL, 2, *dy, *dx, 0) }, None);
            }
            Input::Key { code, down, mods } => {
                let Some((mac, _, _)) = key_of(code) else { return Ok(()) };
                match down {
                    Some(d) => key(mac, *d, *mods),
                    None => {
                        key(mac, true, *mods);
                        key(mac, false, *mods);
                    }
                }
            }
            Input::Text(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                for chunk in units.chunks(UNICODE_CHUNK) {
                    for down in [true, false] {
                        let e = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 0, down) };
                        if !e.is_null() {
                            unsafe { CGEventKeyboardSetUnicodeString(e, chunk.len(), chunk.as_ptr()) };
                        }
                        post(e, Some(0));
                    }
                }
            }
        }
        Ok(())
    }

    /// 시스템 설정의 손쉬운 사용 목록에 카사텀을 올리고 그 기기 앞에 창을 띄운다. 켜면 다시 열 필요 없이 바로 먹는다.
    pub(super) fn request_permission() {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            );
            if !options.is_null() {
                AXIsProcessTrustedWithOptions(options);
                CFRelease(options);
            }
        }
    }
}

#[cfg(windows)]
mod os {
    use super::{key_of, Denied, Input};
    use std::sync::atomic::{AtomicU8, Ordering};
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, MAPVK_VK_TO_VSC,
        VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    /// 휠 한 칸(120)이 대략 몇 픽셀인가 — 보는 쪽은 픽셀로 보내고 Windows 는 칸으로 받는다.
    const PIXELS_PER_NOTCH: i32 = 40;

    /// `screen_capture` 의 화면 번호와 같은 차례(`EnumDisplayMonitors`).
    fn monitor(display: u32) -> Option<RECT> {
        unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
            let list = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
                list.push(info.rcMonitor);
            }
            true.into()
        }
        let mut list: Vec<RECT> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut list as *mut Vec<RECT> as isize));
        }
        list.get(display as usize).or(list.first()).copied()
    }

    fn send(inputs: &[INPUT]) {
        unsafe {
            SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// 단추를 누르는 동안 쥔 손가락(1 shift·2 control·4 alt·8 windows).
    static CLICK_MODS: AtomicU8 = AtomicU8::new(0);

    fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
        }
    }

    fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
            },
        }
    }

    /// 가상 화면 전체를 0–65535 로 편 절대 좌표 — 여러 모니터·음수 원점도 한 식이다.
    fn absolute(display: u32, x: f64, y: f64) -> Option<(i32, i32)> {
        let r = monitor(display)?;
        let (vx, vy) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN)) };
        let (vw, vh) = unsafe { (GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
        if vw <= 1 || vh <= 1 {
            return None;
        }
        let px = f64::from(r.left) + x * f64::from((r.right - r.left - 1).max(0));
        let py = f64::from(r.top) + y * f64::from((r.bottom - r.top - 1).max(0));
        Some((
            ((px - f64::from(vx)) * 65535.0 / f64::from(vw - 1)).round() as i32,
            ((py - f64::from(vy)) * 65535.0 / f64::from(vh - 1)).round() as i32,
        ))
    }

    fn move_to(display: u32, x: f64, y: f64) -> Option<INPUT> {
        let (ax, ay) = absolute(display, x, y)?;
        Some(mouse(ax, ay, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK))
    }

    /// 손가락 비트 → (VK, 확장 키인가). Windows 키는 E0 이 붙는 확장 키다.
    fn modifiers(mods: u8) -> Vec<(u16, bool)> {
        [(1u8, 0x10u16, false), (2, 0x11, false), (4, 0x12, false), (8, 0x5B, true)]
            .iter()
            .filter(|(b, _, _)| mods & b != 0)
            .map(|(_, vk, ext)| (*vk, *ext))
            .collect()
    }

    /// VK 와 스캔 코드를 같이 실은 키 — 게임처럼 키보드를 원시 입력으로 읽는 앱은 VK 가 아니라 스캔 코드를 보고, 0 이면
    /// 아무 키도 안 눌린 것으로 친다(마우스는 먹는데 키만 안 먹던 까닭).
    fn press(vk: u16, extended: bool, up: bool) -> INPUT {
        // PrtSc 의 VK→스캔은 Alt+PrtSc 의 SysRq(0x54)로 나온다. 실제 키처럼 확장 0x37 을 실어야 게임이 PrtSc 로 받는다.
        let scan = if vk == 0x2C { 0x37 } else { (unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC) }) as u16 };
        let mut flags = if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        key(vk, scan, flags)
    }

    pub(super) fn put(display: u32, input: &Input) -> Result<(), Denied> {
        match input {
            Input::Pointer { x, y, buttons, prev, mods } => {
                let Some(go) = move_to(display, *x, *y) else { return Err(Denied::Unavailable) };
                let mut out = vec![go];
                let (down, up, now) = super::click_mods(*prev, *buttons, *mods, CLICK_MODS.load(Ordering::Relaxed));
                CLICK_MODS.store(now, Ordering::Relaxed);
                out.extend(modifiers(down).iter().map(|(m, ext)| press(*m, *ext, false)));
                let changed = buttons ^ prev;
                for (mask, down, up) in [
                    (1u8, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                    (2, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
                    (4, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
                ] {
                    if changed & mask != 0 {
                        out.push(mouse(0, 0, 0, if buttons & mask != 0 { down } else { up }));
                    }
                }
                out.extend(modifiers(up).iter().rev().map(|(m, ext)| press(*m, *ext, true)));
                send(&out);
            }
            Input::Scroll { x, y, dx, dy } => {
                let Some(go) = move_to(display, *x, *y) else { return Err(Denied::Unavailable) };
                let mut out = vec![go];
                if *dy != 0 {
                    out.push(mouse(0, 0, dy * 120 / PIXELS_PER_NOTCH, MOUSEEVENTF_WHEEL));
                }
                if *dx != 0 {
                    out.push(mouse(0, 0, -dx * 120 / PIXELS_PER_NOTCH, MOUSEEVENTF_HWHEEL));
                }
                send(&out);
            }
            Input::Key { code, down, mods } => {
                if code == "Delete" && mods & (2 | 4) == (2 | 4) && *down != Some(false) && send_sas() {
                    return Ok(());
                }
                let Some((_, vk, extended)) = key_of(code) else { return Ok(()) };
                let held = modifiers(*mods);
                let mut out: Vec<INPUT> = Vec::new();
                if *down != Some(false) {
                    out.extend(held.iter().map(|(m, ext)| press(*m, *ext, false)));
                    out.push(press(vk, extended, false));
                }
                if *down != Some(true) {
                    out.push(press(vk, extended, true));
                    out.extend(held.iter().rev().map(|(m, ext)| press(*m, *ext, true)));
                }
                send(&out);
            }
            Input::Text(text) => {
                let out: Vec<INPUT> = text
                    .encode_utf16()
                    .flat_map(|u| [key(0, u, KEYEVENTF_UNICODE), key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)])
                    .collect();
                send(&out);
            }
        }
        Ok(())
    }

    pub(super) fn request_permission() {}

    /// Ctrl+Alt+Del — Windows 는 넣은 키로는 보안 주의 시퀀스(SAS)를 안 받아 `SendSAS` 로 청한다. SYSTEM 화면 도우미에서만
    /// 먹고, 정책 `SoftwareSASGeneration`(설치기가 1 = 서비스로 건다)이 있어야 한다. 못 하면 false — 키를 그대로 넣는다.
    fn send_sas() -> bool {
        use windows::core::{s, w};
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        let Ok(lib) = (unsafe { LoadLibraryW(w!("sas.dll")) }) else { return false };
        let Some(proc) = (unsafe { GetProcAddress(lib, s!("SendSAS")) }) else { return false };
        // SAFETY: sas.dll 의 `VOID WINAPI SendSAS(BOOL AsUser)` 꼴.
        let send: unsafe extern "system" fn(i32) = unsafe { std::mem::transmute(proc) };
        unsafe { send(0) };
        true
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use super::{Denied, Input};

    pub(super) fn put(_: u32, _: &Input) -> Result<(), Denied> {
        Err(Denied::Unavailable)
    }

    pub(super) fn request_permission() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_modifiers_are_held_from_the_press_until_every_button_is_up() {
        assert_eq!(click_mods(0, 1, 4, 0), (4, 0, 4), "⌘(Alt)+누름 — Alt 를 먼저 누른다");
        assert_eq!(click_mods(1, 1, 0, 4), (0, 0, 4), "끄는 동안 이쪽 손가락을 떼도 쥔 채");
        assert_eq!(click_mods(1, 3, 0, 4), (0, 0, 4), "두 번째 단추는 손가락을 다시 안 누른다");
        assert_eq!(click_mods(3, 0, 0, 4), (0, 4, 0), "단추를 다 떼면 쥔 손가락을 뗀다");
        assert_eq!(click_mods(0, 1, 0, 0), (0, 0, 0), "손가락 없는 클릭");
        assert_eq!(click_mods(0, 0, 4, 0), (0, 0, 0), "손가락만 쥐고 움직이기 — 누르지 않는다");
        assert_eq!(click_mods(0, 1, 6, 2), (4, 0, 6), "남은 손가락은 다시 안 누르고 함께 쥔다");
    }

    #[test]
    fn web_key_names_map_to_both_platforms() {
        assert_eq!(key_of("KeyA"), Some((0, 0x41, false)));
        assert_eq!(key_of("KeyZ"), Some((6, 0x5A, false)));
        assert_eq!(key_of("KeyC"), Some((8, 0x43, false)));
        assert_eq!(key_of("Digit0"), Some((29, 0x30, false)));
        assert_eq!(key_of("Digit5"), Some((23, 0x35, false)));
        assert_eq!(key_of("F12"), Some((111, 0x7B, false)));
        assert_eq!(key_of("Enter"), Some((36, 0x0D, false)));
        assert_eq!(key_of("MetaLeft"), Some((55, 0x5B, true)), "키 메뉴의 Windows 키·⌥ 혼자 누르기");
        assert_eq!(key_of("AltLeft"), Some((58, 0x12, false)), "⌘ 혼자 누르기 — Windows Alt");
        assert_eq!(key_of("ArrowUp"), Some((126, 0x26, true)));
        assert_eq!(key_of("Lang1"), Some((104, 0x15, false)), "맥 Caps Lock 이 보내는 Windows 한/영");
        assert_eq!(key_of("PrintScreen"), Some((105, 0x2C, true)), "키 메뉴·PC 키보드 PrtSc(맥에선 F13)");
        assert_eq!(key_of("Numpad7"), Some((89, 0x67, false)));
        assert_eq!(key_of("NumpadEnter"), Some((76, 0x0D, true)));
        for unknown in ["", "Key", "KeyAA", "Keya", "F13", "F0", "Digit10", "Numpad10", "ShiftLeft", "CapsLock"] {
            assert_eq!(key_of(unknown), None, "{unknown}");
        }
    }

    /// 실제 HID 길로 커서를 3포인트 옮겼다 되돌린다 — 화면 사각형·좌표·게시 FFI 가 맞는지. 손쉬운 사용 권한이
    /// 있는 터미널에서 `cargo test -p kasaterm --bin kasaterm screen_input::tests::pointer_lands -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "실제 커서에 이벤트를 넣는다"]
    fn pointer_lands_where_the_fraction_says() {
        #[repr(C)]
        struct P {
            x: f64,
            y: f64,
        }
        #[repr(C)]
        struct R {
            o: P,
            s: P,
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGGetDisplaysWithPoint(point: P, max: u32, displays: *mut u32, count: *mut u32) -> i32;
            fn CGDisplayBounds(display: u32) -> R;
            fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CGEventGetLocation(event: *mut std::ffi::c_void) -> P;
        }
        let here = || unsafe { CGEventGetLocation(CGEventCreate(std::ptr::null())) };
        // 커서가 있는 화면 기준이어야 한다 — 주 화면 기준으로 풀면 옆 모니터의 커서가 주 화면 끝으로 끌려온다.
        let now = here();
        let (mut main, mut count) = (0u32, 0u32);
        unsafe { CGGetDisplaysWithPoint(P { x: now.x, y: now.y }, 1, &mut main, &mut count) };
        assert_eq!(count, 1);
        let b = unsafe { CGDisplayBounds(main) };
        let step = if now.x - b.o.x > b.s.x / 2.0 { -3.0 } else { 3.0 };
        let (x, y) = ((now.x + step - b.o.x) / b.s.x, (now.y - b.o.y) / b.s.y);
        put(main, &Input::Pointer { x, y, buttons: 0, prev: 0, mods: 0 }).expect("손쉬운 사용 권한");
        std::thread::sleep(std::time::Duration::from_millis(50));
        let moved = here();
        let back = ((now.x - b.o.x) / b.s.x, y);
        put(main, &Input::Pointer { x: back.0, y: back.1, buttons: 0, prev: 0, mods: 0 }).unwrap();
        assert!((moved.x - (now.x + step)).abs() < 1.5 && (moved.y - now.y).abs() < 1.5, "{} {} → {} {}", now.x, now.y, moved.x, moved.y);
    }

    #[test]
    fn clicks_count_up_only_when_quick_and_close() {
        let mut slot = None;
        assert_eq!(Clicks::press(&mut slot, 10.0, 10.0), 1);
        assert_eq!(Clicks::press(&mut slot, 12.0, 11.0), 2);
        assert_eq!(Clicks::press(&mut slot, 12.0, 11.0), 3);
        assert_eq!(Clicks::press(&mut slot, 12.0, 11.0), 3);
        assert_eq!(Clicks::press(&mut slot, 40.0, 11.0), 1, "멀리 누르면 새로 센다");
    }
}
