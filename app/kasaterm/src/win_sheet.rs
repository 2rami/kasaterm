//! Windows 의 결정 시트 — macOS `NSAlert` 시트 자리(원격 승인 `remote_approval.rs`, 폰 열쇠 믿기
//! `native_op_approval.rs`).
//!
//! 본창에 붙은 소유 창이다. 열린 동안 본창을 막아 시트처럼 굴고, 메시지는 winit 루프가 그대로 펌프하므로 앱은
//! 계속 그린다 — TaskDialog 처럼 자체 모달 루프를 돌면 그동안 칸 화면이 멎는다. 답은 단추 차례로 돌려준다:
//! 0 은 주 동작(허락·믿기, 맥의 첫 단추), 1 은 기본 동작(거절·취소, Enter). 결정 없이 닫히면 None.
//!
//! winit 루프는 `IsDialogMessage` 를 부르지 않는다. 그래서 Tab·Enter·Esc 는 칸마다 창 프로시저를 갈아 끼워
//! 직접 받는다. 실행 파일에 공용 컨트롤 v6 매니페스트가 없어, 칸을 만들 때만 v6 활성화 문맥을 켠다 — 없으면
//! 원문 칸이 옛 회색 테두리·스크롤바로 그려져 어두운 판에서 튄다.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    AddFontMemResourceEx, BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontIndirectW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetDC, GetDIBits,
    GetMonitorInfoW, GetStockObject, InvalidateRect, MonitorFromWindow, RedrawWindow, ReleaseDC, RoundRect,
    SelectObject, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
    SetBkColor, SetBkMode, SetTextColor, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DT_CALCRECT,
    DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, HBRUSH, HDC, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS, PW_CLIENTONLY};
use windows::Win32::System::ApplicationInstallationAndServicing::{
    ActivateActCtx, CreateActCtxW, DeactivateActCtx, ACTCTXW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    SetWindowTheme, DRAWITEMSTRUCT, EM_SETLIMITTEXT, EM_SETRECT, EM_SETSEL, ODS_DISABLED, ODS_FOCUS, ODS_SELECTED,
    WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL,
    VK_ESCAPE, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClassNameW, GetClientRect, GetParent,
    GetWindowLongPtrW, GetWindowRect, IsWindow, LoadCursorW, RegisterClassExW, SendMessageW, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowTextW, ShowWindow, BS_OWNERDRAW, CS_HREDRAW, CS_VREDRAW, ES_AUTOVSCROLL,
    ES_MULTILINE, ES_READONLY, GWLP_USERDATA, GWLP_WNDPROC, HMENU, IDC_ARROW, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WA_INACTIVE, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_ACTIVATE, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DRAWITEM,
    WM_ERASEBKGND, WM_KEYDOWN, WM_MOUSEMOVE, WM_NCDESTROY, WM_PAINT, WM_SETFONT, WNDCLASSEXW, WNDPROC,
    WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::theme;

/// 시트 한 장의 글과 단추.
pub(crate) struct Spec<'a> {
    pub(crate) title: &'a str,
    pub(crate) info: &'a str,
    /// 고정폭 읽기 전용 원문 칸. None 이면 칸 없이 안내만 선다.
    pub(crate) body: Option<&'a str>,
    /// [주 동작, 기본 동작]. 주 동작은 오른쪽 끝에 위험색으로, 기본 동작은 그 왼쪽에 테두리 단추로 선다.
    pub(crate) buttons: [&'a str; 2],
    pub(crate) primary_enabled: bool,
}

const CLASS: PCWSTR = w!("KasatermSheet");
const ID_BUTTON: i32 = 100;
const ID_BODY: i32 = 110;

/// 열린 시트 수 — 둘이 겹쳐 열렸다가 하나가 닫혀도 본창이 먼저 풀리지 않게.
static OPEN: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy)]
struct Colors {
    bg: [u8; 4],
    text: [u8; 4],
    dim: [u8; 4],
    body_bg: [u8; 4],
    edge: [u8; 4],
    primary: [u8; 4],
    raised: [u8; 4],
    focus: [u8; 4],
    mute: [u8; 4],
}

impl Colors {
    fn now() -> Self {
        let bg = theme::panel_bg();
        Colors {
            bg,
            text: theme::text(),
            dim: theme::text_dim(),
            body_bg: theme::surface(),
            edge: theme::edge_on(bg),
            primary: theme::danger(),
            raised: theme::surface_active(),
            focus: theme::accent(),
            mute: theme::text_mute(),
        }
    }
}

fn rgb(c: [u8; 4]) -> COLORREF {
    COLORREF(c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16)
}

fn mix(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), 255]
}

fn is_dark(c: [u8; 4]) -> bool {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) < 128.0
}

struct Sheet {
    owner: HWND,
    answer: Box<dyn Fn(Option<usize>)>,
    title: Vec<u16>,
    info: Vec<u16>,
    title_rect: RECT,
    info_rect: RECT,
    /// 원문 칸 테두리 사각형(칸은 그 안 1px).
    body_rect: Option<RECT>,
    edit: HWND,
    buttons: [HWND; 2],
    labels: [Vec<u16>; 2],
    primary_enabled: bool,
    title_font: HFONT,
    text_font: HFONT,
    button_font: HFONT,
    mono_font: HFONT,
    bg_brush: HBRUSH,
    body_brush: HBRUSH,
    colors: Colors,
    radius: i32,
    ring: i32,
    hot: Option<usize>,
    /// 다시 초점을 받을 때 돌려줄 칸.
    focus: HWND,
    done: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn hwnd_of(window: &winit::window::Window) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None) }.map(Into::into).unwrap_or_default()
}

fn register_class() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(sheet_proc),
            hInstance: instance(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    });
}

/// 공용 컨트롤 v6 활성화 문맥. 매니페스트를 파일로 두고 한 번만 만든다.
fn comctl6() -> Option<HANDLE> {
    static CTX: OnceLock<Option<isize>> = OnceLock::new();
    let raw = *CTX.get_or_init(|| {
        let dir = std::env::temp_dir().join("kasaterm-sheet");
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("comctl6.manifest");
        std::fs::write(
            &path,
            concat!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
                r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">"#,
                r#"<dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" "#,
                r#"version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>"#,
                r#"</dependentAssembly></dependency></assembly>"#
            ),
        )
        .ok()?;
        let source = HSTRING::from(path.as_os_str());
        let ctx = ACTCTXW {
            cbSize: std::mem::size_of::<ACTCTXW>() as u32,
            lpSource: PCWSTR(source.as_ptr()),
            ..Default::default()
        };
        unsafe { CreateActCtxW(&ctx) }.ok().map(|h| h.0 as isize)
    });
    raw.map(|r| HANDLE(r as *mut core::ffi::c_void))
}

/// 원문 칸 글꼴 — 칸 격자와 같은 D2Coding 을 이 프로세스에만 올린다. 한글이 고정폭으로 줄을 맞춘다.
fn mono_face() -> &'static str {
    static ADDED: OnceLock<bool> = OnceLock::new();
    let added = *ADDED.get_or_init(|| {
        let data = kasa_gridview::fonts::d2coding_regular();
        let mut count = 0u32;
        let handle = unsafe { AddFontMemResourceEx(data.as_ptr().cast(), data.len() as u32, None, &mut count) };
        !handle.is_invalid() && count > 0
    });
    if added {
        "D2Coding"
    } else {
        "Consolas"
    }
}

/// 시스템 대화 글꼴(한국어 Windows 면 맑은 고딕)을 크기·굵기만 바꿔 쓴다.
#[derive(Clone, Copy)]
enum Weight {
    Regular = 400,
    Bold = 700,
}

fn ui_font(dpi: u32, px: i32, weight: Weight, face: Option<&str>) -> HFONT {
    unsafe {
        let mut metrics = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some(&mut metrics as *mut _ as *mut core::ffi::c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0).0,
            dpi,
        );
        let mut font = metrics.lfMessageFont;
        font.lfHeight = -px;
        font.lfWeight = weight as i32;
        if let Some(face) = face {
            font.lfFaceName = [0; 32];
            for (i, c) in face.encode_utf16().take(31).enumerate() {
                font.lfFaceName[i] = c;
            }
        }
        CreateFontIndirectW(&font)
    }
}

fn text_height(dc: HDC, font: HFONT, text: &[u16], width: i32) -> i32 {
    let mut rect = RECT { left: 0, top: 0, right: width, bottom: 0 };
    let mut text = text.to_vec();
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(dc, &mut text, &mut rect, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(dc, old);
    }
    rect.bottom
}

fn text_width(dc: HDC, font: HFONT, text: &[u16]) -> i32 {
    let mut rect = RECT::default();
    let mut text = text.to_vec();
    unsafe {
        let old = SelectObject(dc, font.into());
        DrawTextW(dc, &mut text, &mut rect, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old);
    }
    rect.right
}

/// 시트를 세운다. 돌려받는 손잡이로 [`press`]·[`shot`]·[`close`] 를 부른다.
pub(crate) fn open(window: &winit::window::Window, spec: &Spec, answer: impl Fn(Option<usize>) + 'static) -> Option<usize> {
    let owner = hwnd_of(window)?;
    register_class();
    let dpi = unsafe { GetDpiForWindow(owner) }.max(96);
    let px = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
    let colors = Colors::now();
    let title_font = ui_font(dpi, px(15.0), Weight::Bold, None);
    let text_font = ui_font(dpi, px(13.0), Weight::Regular, None);
    let button_font = ui_font(dpi, px(13.0), Weight::Bold, None);
    let mono_font = ui_font(dpi, px(13.0), Weight::Regular, Some(mono_face()));

    let (title, info) = (wide(spec.title), wide(spec.info));
    let labels = [wide(spec.buttons[0]), wide(spec.buttons[1])];
    let (pad, width) = (px(20.0), px(600.0));
    let inner = width - pad * 2;
    let (title_h, info_h, label_w) = unsafe {
        let dc = GetDC(None);
        let measured = (
            text_height(dc, title_font, &title, inner),
            text_height(dc, text_font, &info, inner),
            [text_width(dc, button_font, &labels[0]), text_width(dc, button_font, &labels[1])],
        );
        ReleaseDC(None, dc);
        measured
    };
    let mut y = pad;
    let title_rect = RECT { left: pad, top: y, right: pad + inner, bottom: y + title_h };
    y += title_h + px(6.0);
    let info_rect = RECT { left: pad, top: y, right: pad + inner, bottom: y + info_h };
    y += info_h + px(14.0);
    let body_rect = spec.body.map(|_| {
        let r = RECT { left: pad, top: y, right: pad + inner, bottom: y + px(260.0) };
        y += px(260.0) + px(18.0);
        r
    });
    if body_rect.is_none() {
        y += px(4.0);
    }
    let button_h = px(32.0);
    let button_w = label_w.map(|w| (w + px(36.0)).max(px(88.0)));
    let primary_x = pad + inner - button_w[0];
    let default_x = primary_x - px(8.0) - button_w[1];
    let button_y = y;
    y += button_h + pad;

    // 자식 칸을 오려 내고 칠한다 — 없으면 바탕 칠하기가 먼저 그려진 원문 칸·단추를 덮고, 자식은 다시 그릴 일을 모른다.
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
    let ex = WS_EX_DLGMODALFRAME;
    let mut frame = RECT { left: 0, top: 0, right: width, bottom: y };
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut frame, style, false, ex, dpi);
    }
    let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
    let (x, top) = centered_over(owner, w, h);

    let sheet = Box::new(Sheet {
        owner,
        answer: Box::new(answer),
        title,
        info,
        title_rect,
        info_rect,
        body_rect,
        edit: HWND::default(),
        buttons: [HWND::default(); 2],
        labels,
        primary_enabled: spec.primary_enabled,
        title_font,
        text_font,
        button_font,
        mono_font,
        bg_brush: unsafe { CreateSolidBrush(rgb(colors.bg)) },
        body_brush: unsafe { CreateSolidBrush(rgb(colors.body_bg)) },
        colors,
        radius: (theme::radius_sm() * 2.0 * dpi as f32 / 96.0).round() as i32,
        ring: px(2.0),
        hot: None,
        focus: HWND::default(),
        done: false,
    });

    unsafe {
        // 제목줄은 앱 이름 — 물음은 안쪽 제목이 말한다(같은 문장이 두 번 서지 않게).
        let hwnd = CreateWindowExW(
            ex,
            CLASS,
            w!("kasaterm"),
            style,
            x,
            top,
            w,
            h,
            Some(owner),
            None,
            Some(instance()),
            None,
        )
        .ok()?;
        let state = Box::into_raw(sheet);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        let dark = windows::core::BOOL::from(is_dark(colors.bg));
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        // 제목줄 색은 Windows 11 부터다 — 그 전에는 위 어두운 제목줄까지만 맞는다.
        let caption = rgb(colors.bg);
        let caption_text = rgb(colors.text);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &caption_text as *const _ as _, 4);

        let mut cookie = 0usize;
        let ctx = comctl6();
        let activated = ctx.is_some_and(|c| ActivateActCtx(Some(c), &mut cookie).is_ok());
        if let (Some(body), Some(text)) = (body_rect, spec.body) {
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                None,
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32),
                body.left + 1,
                body.top + 1,
                body.right - body.left - 2,
                body.bottom - body.top - 2,
                Some(hwnd),
                Some(HMENU(ID_BODY as usize as *mut _)),
                Some(instance()),
                None,
            )
            .unwrap_or_default();
            if is_dark(colors.body_bg) {
                let _ = SetWindowTheme(edit, w!("DarkMode_Explorer"), PCWSTR::null());
            }
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(mono_font.0 as usize)), Some(LPARAM(0)));
            // 원문은 64KB 까지 온다 — 기본 상한(약 3만 자)에 걸려 뒤가 잘리지 않게 푼다.
            SendMessageW(edit, EM_SETLIMITTEXT, Some(WPARAM(0)), Some(LPARAM(0)));
            let _ = SetWindowTextW(edit, &HSTRING::from(text.replace('\n', "\r\n")));
            let mut inset = RECT { left: px(8.0), top: px(6.0), right: body.right - body.left - 2 - px(8.0), bottom: body.bottom - body.top - 2 - px(6.0) };
            let mut client = RECT::default();
            if GetClientRect(edit, &mut client).is_ok() {
                inset.right = client.right - px(8.0);
                inset.bottom = client.bottom - px(6.0);
            }
            SendMessageW(edit, EM_SETRECT, Some(WPARAM(0)), Some(LPARAM(&inset as *const RECT as isize)));
            subclass(edit);
            (*state).edit = edit;
        }
        for (i, (bx, bw)) in [(primary_x, button_w[0]), (default_x, button_w[1])].into_iter().enumerate() {
            let button = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                &HSTRING::from(spec.buttons[i]),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                bx,
                button_y,
                bw,
                button_h,
                Some(hwnd),
                Some(HMENU((ID_BUTTON + i as i32) as usize as *mut _)),
                Some(instance()),
                None,
            )
            .unwrap_or_default();
            subclass(button);
            (*state).buttons[i] = button;
        }
        if activated {
            let _ = DeactivateActCtx(0, cookie);
        }
        if !spec.primary_enabled {
            let _ = EnableWindow((*state).buttons[0], false);
        }
        (*state).focus = (*state).buttons[1];
        OPEN.fetch_add(1, Ordering::Relaxed);
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some((*state).buttons[1]));
        Some(hwnd.0 as usize)
    }
}

/// 본창 가운데, 화면 작업 영역 안으로.
fn centered_over(owner: HWND, w: i32, h: i32) -> (i32, i32) {
    unsafe {
        let mut rect = RECT::default();
        let _ = GetWindowRect(owner, &mut rect);
        let mut x = rect.left + (rect.right - rect.left - w) / 2;
        let mut y = rect.top + (rect.bottom - rect.top - h) / 2;
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            let area = info.rcWork;
            x = x.clamp(area.left, (area.right - w).max(area.left));
            y = y.clamp(area.top, (area.bottom - h).max(area.top));
        }
        (x, y)
    }
}

fn state(hwnd: HWND) -> *mut Sheet {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Sheet }
}

/// 손잡이가 아직 살아 있는 우리 시트인가 — 닫힌 창 번호는 다른 창에 다시 쓰일 수 있다.
fn sheet_of(handle: usize) -> Option<HWND> {
    let hwnd = HWND(handle as *mut core::ffi::c_void);
    unsafe {
        if handle == 0 || !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let mut name = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut name) as usize;
        (String::from_utf16_lossy(&name[..n]) == "KasatermSheet" && !state(hwnd).is_null()).then_some(hwnd)
    }
}

/// 답을 내고 시트를 거둔다. 거두기 **전에** 본창을 풀어야 초점이 다른 앱이 아니라 본창으로 돌아간다.
/// DestroyWindow 가 상태를 지우므로 그 뒤로는 상태를 만지지 않는다.
unsafe fn finish(hwnd: HWND, choice: Option<usize>) {
    let st = state(hwnd);
    if st.is_null() || unsafe { (*st).done } {
        return;
    }
    if choice == Some(0) && !unsafe { (*st).primary_enabled } {
        return;
    }
    unsafe {
        (*st).done = true;
        ((*st).answer)(choice);
        let owner = (*st).owner;
        if OPEN.fetch_sub(1, Ordering::Relaxed) == 1 {
            let _ = EnableWindow(owner, true);
        }
        let _ = DestroyWindow(hwnd);
    }
}

pub(crate) fn press(handle: usize, index: usize) {
    if let Some(hwnd) = sheet_of(handle) {
        unsafe { finish(hwnd, Some(index)) }
    }
}

/// 다른 곳에서 닫힌 요청의 시트를 결정 없이 거둔다.
pub(crate) fn close(handle: usize) {
    if let Some(hwnd) = sheet_of(handle) {
        unsafe { finish(hwnd, None) }
    }
}

/// 검증 실행이 시트 창을 PNG 로 — 화면 캡처가 아니라 가려져 있어도 뜬다.
///
/// 제목줄은 DWM 이 합성한 그림(PW_RENDERFULLCONTENT, 어두운 제목줄 색 포함)에서, 안쪽은 창에게 다시 그리게 한
/// 그림(PW_CLIENTONLY)에서 가져온다. DWM 그림만 쓰면 다른 창에 가려진 동안의 원문 칸이 몇 줄에서 끊겨 찍혀,
/// 화면은 멀쩡한데 「덜 그려진다」로 오판한다(2026-10-23 실측).
pub(crate) fn shot(handle: usize, path: &str) {
    let Some(hwnd) = sheet_of(handle) else { return };
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(Some(screen));
        let bitmap = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(dc, bitmap.into());
        let _ = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2));
        let mut client = RECT::default();
        let mut origin = POINT::default();
        if GetClientRect(hwnd, &mut client).is_ok() && ClientToScreen(hwnd, &mut origin).as_bool() {
            let inner = CreateCompatibleDC(Some(screen));
            let inner_bitmap = CreateCompatibleBitmap(screen, client.right, client.bottom);
            let inner_old = SelectObject(inner, inner_bitmap.into());
            if PrintWindow(hwnd, inner, PW_CLIENTONLY).as_bool() {
                let _ = BitBlt(dc, origin.x - rect.left, origin.y - rect.top, client.right, client.bottom, Some(inner), 0, 0, SRCCOPY);
            }
            SelectObject(inner, inner_old);
            let _ = DeleteObject(inner_bitmap.into());
            let _ = DeleteDC(inner);
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        SelectObject(dc, old);
        let lines = GetDIBits(dc, bitmap, 0, h as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        ReleaseDC(None, screen);
        if lines == 0 {
            return;
        }
        for p in pixels.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        if let Some(image) = image::RgbaImage::from_raw(w as u32, h as u32, pixels) {
            let _ = image.save(path);
        }
    }
}

/// 칸의 창 프로시저를 갈아 끼운다. 원래 것은 그 칸의 사용자 자리에 둔다(EDIT·BUTTON 은 그 자리를 안 쓴다).
unsafe fn subclass(control: HWND) {
    unsafe {
        let old = SetWindowLongPtrW(control, GWLP_WNDPROC, control_proc as *const () as isize);
        SetWindowLongPtrW(control, GWLP_USERDATA, old);
    }
}

fn tab_order(st: &Sheet) -> Vec<HWND> {
    let mut order = Vec::new();
    if !st.edit.is_invalid() {
        order.push(st.edit);
    }
    if st.primary_enabled {
        order.push(st.buttons[0]);
    }
    order.push(st.buttons[1]);
    order
}

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let old: WNDPROC = std::mem::transmute(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        let sheet = GetParent(hwnd).unwrap_or_default();
        let st = state(sheet);
        if st.is_null() {
            return CallWindowProcW(old, hwnd, msg, wp, lp);
        }
        match msg {
            WM_KEYDOWN => {
                let key = wp.0 as u16;
                // Enter 는 언제나 거절·취소다 — 터미널에서 치던 Enter 가 시트로 새도 허락이 나가지 않게.
                if key == VK_RETURN.0 {
                    finish(sheet, Some(1));
                    return LRESULT(0);
                }
                if key == VK_ESCAPE.0 {
                    finish(sheet, None);
                    return LRESULT(0);
                }
                if key == VK_TAB.0 {
                    let order = tab_order(&*st);
                    let at = order.iter().position(|h| *h == hwnd).unwrap_or(0);
                    let back = GetKeyState(VK_SHIFT.0 as i32) < 0;
                    let next = if back { (at + order.len() - 1) % order.len() } else { (at + 1) % order.len() };
                    let _ = SetFocus(Some(order[next]));
                    return LRESULT(0);
                }
                if key == b'A' as u16 && hwnd == (*st).edit && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                    SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                    return LRESULT(0);
                }
            }
            // 위에서 삼킨 키가 TranslateMessage 로 만든 글자 — 읽기 전용 칸이 삑 소리를 낸다.
            WM_CHAR if matches!(wp.0, 0x09 | 0x0D | 0x1B | 0x01) => return LRESULT(0),
            WM_MOUSEMOVE => {
                if let Some(i) = (*st).buttons.iter().position(|b| *b == hwnd) {
                    if (*st).hot != Some(i) {
                        (*st).hot = Some(i);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
            }
            WM_MOUSELEAVE => {
                if (*st).buttons.contains(&hwnd) {
                    (*st).hot = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            _ => {}
        }
        CallWindowProcW(old, hwnd, msg, wp, lp)
    }
}

unsafe fn paint(hwnd: HWND, st: &mut Sheet) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        FillRect(dc, &client, st.bg_brush);
        SetBkMode(dc, TRANSPARENT);
        let old = SelectObject(dc, st.title_font.into());
        SetTextColor(dc, rgb(st.colors.text));
        let mut rect = st.title_rect;
        DrawTextW(dc, &mut st.title.clone(), &mut rect, DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(dc, st.text_font.into());
        SetTextColor(dc, rgb(st.colors.dim));
        let mut rect = st.info_rect;
        DrawTextW(dc, &mut st.info.clone(), &mut rect, DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(dc, old);
        if let Some(body) = st.body_rect {
            let edge = CreateSolidBrush(rgb(st.colors.edge));
            FrameRect(dc, &body, edge);
            let _ = DeleteObject(edge.into());
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe fn draw_button(st: &Sheet, item: &DRAWITEMSTRUCT) {
    let index = (item.CtlID as i32 - ID_BUTTON) as usize;
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let focused = item.itemState.0 & ODS_FOCUS.0 != 0;
    let hot = st.hot == Some(index) && !disabled;
    let c = st.colors;
    let (fill, fg, edge) = if index == 0 {
        if disabled {
            (mix(c.bg, c.mute, 0.25), c.mute, None)
        } else {
            let base = if pressed { mix(c.primary, [0, 0, 0, 255], 0.15) } else if hot { mix(c.primary, [255; 4], 0.12) } else { c.primary };
            (base, [255; 4], None)
        }
    } else {
        let base = if pressed { mix(c.raised, [0, 0, 0, 255], 0.12) } else { theme::raised_on(c.raised, hot) };
        (base, c.text, Some(theme::edge_on(c.raised)))
    };
    unsafe {
        let dc = item.hDC;
        let rect = item.rcItem;
        FillRect(dc, &rect, st.bg_brush);
        let brush = CreateSolidBrush(rgb(fill));
        let pen = CreatePen(PS_SOLID, 1, rgb(edge.unwrap_or(fill)));
        let old_brush = SelectObject(dc, brush.into());
        let old_pen = SelectObject(dc, pen.into());
        let _ = RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, st.radius, st.radius);
        if focused {
            let ring = CreatePen(PS_SOLID, st.ring, rgb(c.focus));
            SelectObject(dc, ring.into());
            SelectObject(dc, GetStockObject(NULL_BRUSH));
            let i = st.ring / 2;
            let _ = RoundRect(dc, rect.left + i, rect.top + i, rect.right - i + 1, rect.bottom - i + 1, st.radius, st.radius);
            SelectObject(dc, pen.into());
            let _ = DeleteObject(ring.into());
        }
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        let _ = DeleteObject(brush.into());
        let _ = DeleteObject(pen.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, rgb(fg));
        let old_font = SelectObject(dc, st.button_font.into());
        let mut text_rect = rect;
        DrawTextW(dc, &mut st.labels[index].clone(), &mut text_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(dc, old_font);
    }
}

unsafe extern "system" fn sheet_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let st = state(hwnd);
        if st.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        match msg {
            WM_PAINT => {
                paint(hwnd, &mut *st);
                return LRESULT(0);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT if HWND(lp.0 as *mut _) == (*st).edit => {
                let dc = HDC(wp.0 as *mut _);
                SetTextColor(dc, rgb((*st).colors.text));
                SetBkColor(dc, rgb((*st).colors.body_bg));
                return LRESULT((*st).body_brush.0 as isize);
            }
            WM_DRAWITEM => {
                let item = &*(lp.0 as *const DRAWITEMSTRUCT);
                if (ID_BUTTON..ID_BUTTON + 2).contains(&(item.CtlID as i32)) {
                    draw_button(&*st, item);
                    return LRESULT(TRUE.0 as isize);
                }
            }
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as i32;
                if (ID_BUTTON..ID_BUTTON + 2).contains(&id) {
                    finish(hwnd, Some((id - ID_BUTTON) as usize));
                    return LRESULT(0);
                }
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    let f = GetFocus();
                    if !f.is_invalid() && f != hwnd {
                        (*st).focus = f;
                    }
                } else {
                    let _ = SetFocus(Some((*st).focus));
                }
                return LRESULT(0);
            }
            WM_CLOSE => {
                finish(hwnd, None);
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let sheet = Box::from_raw(st);
                if !sheet.done && OPEN.fetch_sub(1, Ordering::Relaxed) == 1 {
                    let _ = EnableWindow(sheet.owner, true);
                }
                for font in [sheet.title_font, sheet.text_font, sheet.button_font, sheet.mono_font] {
                    let _ = DeleteObject(font.into());
                }
                let _ = DeleteObject(sheet.bg_brush.into());
                let _ = DeleteObject(sheet.body_brush.into());
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}
