//! Windows 알림센터 토스트와 작업표시줄 배지 — macOS 의 `UNUserNotificationCenter`·Dock 배지 자리.
//!
//! 토스트는 앱 신원(AUMID)이 등록돼 있어야 선다. 신원은 HKCU 의 `AppUserModelId` 키로 등록한다 — 시작 메뉴
//! 바로가기 없이도 서므로 MSI 설치본과 포터블 zip 이 같은 길을 탄다. 프로세스에 명시 AUMID 를 걸지 않는 것은
//! 이미 작업표시줄에 고정된 바로가기가 exe 경로로 지은 암묵 신원을 들고 있어서다. 실행 창이 다른 신원을 대면
//! 고정 아이콘과 창이 두 칸으로 갈라진다.
//!
//! 알림센터에 남은 토스트를 누르면 Windows 는 그 신원의 COM 활성화기(`INotificationActivationCallback`)를
//! 부른다. 앱이 떠 있으면 이 프로세스가 등록한 클래스 객체가 받고, 꺼져 있으면 `LocalServer32` 로 exe 를 띄운다.
//! 받은 launch 인자는 macOS 알림 identifier 와 같은 형식이라 `NotifyFocus` 가 같은 세션 검사를 한다.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, OnceLock};

use windows::core::{implement, Interface, Ref, BOOL, GUID, HSTRING, PCWSTR};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Win32::Foundation::{CLASS_E_NOAGGREGATION, COLORREF, E_ACCESSDENIED, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW, GdiFlush,
    SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    FW_BOLD, OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoRegisterClassObject, IClassFactory, IClassFactory_Impl,
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, REGCLS_MULTIPLEUSE,
};
use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Notifications::{
    INotificationActivationCallback, INotificationActivationCallback_Impl, NOTIFICATION_USER_INPUT_DATA,
};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, IsIconic, SetForegroundWindow, ShowWindow, HICON, ICONINFO, SM_CXSMICON,
    SW_RESTORE,
};
use windows::UI::Notifications::{NotificationSetting, ToastNotification, ToastNotificationManager};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// 알림센터에 서는 이름. 설치본과 검증 리그는 신원을 가른다 — 리그가 설치본의 활성화기 자리를 덮으면
/// 사람이 누른 알림이 리그로 간다.
struct Identity {
    aumid: &'static str,
    clsid: GUID,
    name: &'static str,
    /// 꺼진 앱을 알림 클릭으로 띄울지. 리그 exe 는 사람 손으로 다시 뜨면 안 된다.
    launch: bool,
}

const PRODUCT: Identity = Identity {
    aumid: "com.kasa.kasaterm",
    clsid: GUID::from_u128(0xcdda772f_d213_4d8d_b303_047fb859fcf6),
    name: "kasaterm",
    launch: true,
};

const RIG: Identity = Identity {
    aumid: "com.kasa.kasaterm.rig",
    clsid: GUID::from_u128(0xd79515d3_0e66_43c1_b6a6_aff9f85d594d),
    name: "kasaterm 검증",
    launch: false,
};

const APP_ICON: &[u8] = include_bytes!("../../../assets/AppIcon.iconset/icon_256x256.png");

struct Request {
    title: String,
    body: String,
    character: Option<String>,
    route: Option<(String, Option<String>)>,
}

static WORKER: OnceLock<mpsc::Sender<Request>> = OnceLock::new();
/// 알림을 누르면 앞으로 올릴 본창. 활성화기는 COM 스레드에서 불려 `App` 에 닿지 못한다.
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// 사람 화면에 알림을 내면 안 되는 실행 — 검증 리그·격리 env·cargo 빌드 폴더에서 바로 띄운 exe. 맥의
/// 「번들 없는 실행」 자리다(리그 알림이 사람 화면에 뜨던 것, 2026-10-07).
pub(crate) fn quiet_run() -> bool {
    crate::verification_run()
        || crate::version::isolated_environment(|key| std::env::var_os(key).is_some())
        || std::env::current_exe().is_ok_and(|exe| in_cargo_target(&exe))
}

fn in_cargo_target(exe: &Path) -> bool {
    let parts: Vec<String> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    parts.iter().position(|p| p == "target").is_some_and(|at| {
        parts[at + 1..].iter().any(|p| p == "debug" || p == "release")
    })
}

/// 이 실행이 낼 토스트의 신원. 조용한 실행은 `KASATERM_NOTIFY_TOAST` 를 줬을 때만 리그 신원으로 낸다.
fn identity() -> Option<&'static Identity> {
    if crate::lite_mode() {
        return None;
    }
    if !quiet_run() {
        return Some(&PRODUCT);
    }
    std::env::var_os("KASATERM_NOTIFY_TOAST").map(|_| &RIG)
}

fn hwnd_of(window: &winit::window::Window) -> Option<HWND> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// 토스트 일꾼을 세운다. 프로세스당 한 번, **알림이 배달되기 전에** — 활성화기가 없는 동안 눌린 토스트는 갈 곳이
/// 없다. 신원 등록·클래스 등록·배달을 한 MTA 스레드가 맡는다: `notify_desktop` 은 어느 스레드에서도 불리고,
/// WinRT 호출이 주 스레드 프레임을 붙잡지 않게 한다.
pub(crate) fn install(proxy: EventLoopProxy<UserEvent>, window: &winit::window::Window) {
    if let Some(hwnd) = hwnd_of(window) {
        MAIN_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    }
    let Some(id) = identity() else { return };
    if WORKER.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("win-toast".into())
        .spawn(move || run_worker(id, proxy, rx));
    if spawned.is_ok() {
        let _ = WORKER.set(tx);
    }
}

/// 토스트로 낸다. 일꾼이 없으면(조용한 실행·라이트·아직 부팅 전) false를 돌려준다.
pub(crate) fn toast(title: &str, body: &str, character: Option<&str>, route: Option<(&str, Option<&str>)>) -> bool {
    let Some(tx) = WORKER.get() else { return false };
    tx.send(Request {
        title: title.to_string(),
        body: body.to_string(),
        character: character.map(str::to_string),
        route: route.map(|(p, s)| (p.to_string(), s.map(str::to_string))),
    })
    .is_ok()
}

fn run_worker(id: &'static Identity, proxy: EventLoopProxy<UserEvent>, rx: mpsc::Receiver<Request>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    register_identity(id);
    let factory: IClassFactory = Factory { proxy: proxy.clone() }.into();
    // 쿠키는 거두지 않는다 — 프로세스가 끝날 때까지 클릭을 받는다.
    if let Err(e) = unsafe { CoRegisterClassObject(&id.clsid, &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE) } {
        eprintln!("[notify] 토스트 활성화기 등록 실패 — 눌러도 칸으로 못 간다: {e}");
    }
    let mut seq = 0u64;
    for req in rx {
        seq += 1;
        let shown = show(id, &req, seq);
        // OS에서 끈 알림을 앱 배너로 되살리지 않는다.
        if let Err(e) = shown {
            if e.code() != E_ACCESSDENIED { eprintln!("[notify] 알림센터 배달 실패: {e}"); }
        }
    }
}

fn show(id: &Identity, req: &Request, seq: u64) -> windows::core::Result<()> {
    // 알림기는 매번 새로 받는다 — 신원을 적기 전에 받아 둔 알림기는 그 뒤에도 「없는 앱」으로 남는다.
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(id.aumid))?;
    // 사람이 설정에서 kasaterm 알림을 껐거나 정책이 막았으면 Show 는 조용히 버린다 — OS의 선택을 그대로 따른다.
    // 조회 자체가 안 되면 막지 않고 Show 에 맡긴다.
    if notifier.Setting().is_ok_and(|s| s != NotificationSetting::Enabled) {
        return Err(E_ACCESSDENIED.into());
    }
    let launch = crate::chrome::notify_ident(seq, req.route.as_ref().map(|(p, s)| (p.as_str(), s.as_deref())));
    let image = req.character.as_deref().and_then(|c| crate::chrome::student_profile_file(c, None));
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(toast_xml(&req.title, &req.body, image.as_deref(), &launch)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    notifier.Show(&toast)
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 이 못 담는 제어 문자 하나가 문서 전체를 거절시킨다.
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\t') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// 토스트 이미지 src 는 URI 다 — 사용자 이름에 공백·한글이 있어도 깨지지 않게 바이트 단위로 감싼다.
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file:///");
    for b in path.to_string_lossy().replace('\\', "/").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn toast_xml(title: &str, body: &str, image: Option<&Path>, launch: &str) -> String {
    // 학생 얼굴은 앱 아이콘 자리에 둥글게 — macOS 의 대화 알림 아바타와 같은 자리다.
    let image = image
        .map(|p| format!(r#"<image placement="appLogoOverride" hint-crop="circle" src="{}"/>"#, xml_escape(&file_uri(p))))
        .unwrap_or_default();
    format!(
        r#"<toast launch="{}"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>{image}</binding></visual></toast>"#,
        xml_escape(launch),
        xml_escape(title),
        xml_escape(body),
    )
}

fn reg_set(key: &str, name: &str, value: &str) -> bool {
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let name = (!name.is_empty()).then(|| HSTRING::from(name));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr())),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    }
    .is_ok()
}

/// 알림센터가 그릴 이름·아이콘과 클릭을 받을 활성화기를 HKCU 에 적는다. 부팅마다 다시 적어 exe 를 옮긴
/// 포터블 판도 제자리를 가리킨다.
fn register_identity(id: &Identity) {
    let clsid = format!("{{{:?}}}", id.clsid);
    let key = format!(r"Software\Classes\AppUserModelId\{}", id.aumid);
    let mut ok = reg_set(&key, "DisplayName", id.name) && reg_set(&key, "CustomActivator", &clsid);
    if let Some(icon) = icon_file(id) {
        ok &= reg_set(&key, "IconUri", &icon.to_string_lossy());
    }
    if id.launch {
        if let Ok(exe) = std::env::current_exe() {
            ok &= reg_set(
                &format!(r"Software\Classes\CLSID\{clsid}\LocalServer32"),
                "",
                &format!("\"{}\" -ToastActivated", exe.display()),
            );
        }
    }
    if !ok {
        eprintln!("[notify] 알림센터 신원 등록 일부 실패");
    }
}

/// 알림센터 아이콘 — 레지스트리는 exe 안의 그림을 못 가리켜 파일로 꺼내 둔다.
fn icon_file(id: &Identity) -> Option<PathBuf> {
    let dir = if id.launch {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("kasaterm").join("notify")
    } else {
        std::env::temp_dir().join("kasaterm-notify-rig")
    };
    let path = dir.join("app.png");
    if std::fs::metadata(&path).ok().map(|m| m.len()) != Some(APP_ICON.len() as u64) {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&path, APP_ICON).ok()?;
    }
    Some(path)
}

/// 앱이 떠 있는데 클릭 권한이 막혀 있으면 창만 깜빡인다. 토스트 활성화는 이 프로세스에 앞으로 나설 권한을
/// 함께 주므로, 받은 자리(COM 스레드)에서 바로 올린다. 칸 옮기기는 `NotifyFocus` 가 주 스레드에서 한다.
fn bring_forward() {
    let raw = MAIN_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut core::ffi::c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

#[implement(INotificationActivationCallback)]
struct Activator {
    proxy: EventLoopProxy<UserEvent>,
}

impl INotificationActivationCallback_Impl for Activator_Impl {
    fn Activate(
        &self,
        _aumid: &PCWSTR,
        args: &PCWSTR,
        _data: *const NOTIFICATION_USER_INPUT_DATA,
        _count: u32,
    ) -> windows::core::Result<()> {
        bring_forward();
        let args = unsafe { args.to_string() }.unwrap_or_default();
        if let Some((pane, sid)) = crate::chrome::notify_route(&args) {
            let _ = self.proxy.send_event(UserEvent::NotifyFocus { pane, sid });
        }
        Ok(())
    }
}

#[implement(IClassFactory)]
struct Factory {
    proxy: EventLoopProxy<UserEvent>,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, windows::core::IUnknown>,
        iid: *const GUID,
        object: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        unsafe {
            *object = std::ptr::null_mut();
        }
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let callback: INotificationActivationCallback = Activator { proxy: self.proxy.clone() }.into();
        unsafe { callback.query(iid, object).ok() }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

// ── 작업표시줄 배지 ───────────────────────────────────────────────────────────

thread_local! {
    static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
}

/// 안 읽은 알림 수를 작업표시줄 단추 위 겹침 아이콘으로(0 이면 거둔다). Dock 배지 자리다.
/// 주 스레드에서만 부른다 — 작업표시줄 객체는 STA 객체다.
pub(crate) fn set_taskbar_badge(window: Option<&winit::window::Window>, count: usize) {
    let Some(hwnd) = window.and_then(hwnd_of) else { return };
    TASKBAR.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            let list: Option<ITaskbarList3> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }.ok();
            *cell = list.filter(|l| unsafe { l.HrInit() }.is_ok());
        }
        let Some(list) = cell.as_ref() else { return };
        if count == 0 {
            unsafe {
                let _ = list.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null());
            }
            return;
        }
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForWindow(hwnd)) }.max(16);
        let Some(icon) = badge_icon(&badge_label(count), size, crate::theme::danger()) else { return };
        let label = HSTRING::from(format!("안 읽은 알림 {count}개"));
        unsafe {
            let _ = list.SetOverlayIcon(hwnd, icon, &label);
            // 작업표시줄이 제 사본을 만들어 쓴다.
            let _ = DestroyIcon(icon);
        }
    });
}

/// 16px 원에는 두 글자가 한계다.
fn badge_label(count: usize) -> String {
    if count > 9 {
        "9+".to_string()
    } else {
        count.to_string()
    }
}

/// 글자는 GDI 로 흑백 칸에 그려 밝기를 덮임 정도로 쓰고, 원과 섞는 것은 손으로 한다. GDI 는 알파를 안 써서
/// 그 칸을 그대로 아이콘으로 쓰면 글자 둘레가 검게 남는다.
fn badge_icon(label: &str, size: i32, fill: [u8; 4]) -> Option<HICON> {
    let cover = text_coverage(label, size)?;
    let pixels = compose_badge(size as usize, &cover, fill);
    unsafe {
        let (dc, bitmap, bits) = dib(size)?;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits, pixels.len());
        let _ = DeleteDC(dc);
        let mask_row = ((size as usize + 15) / 16) * 2;
        let zeros = vec![0u8; mask_row * size as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(zeros.as_ptr().cast()));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: bitmap };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteObject(mask.into());
        icon
    }
}

/// 위에서 아래로 쌓인 32비트 칸 하나와 그 픽셀 자리.
unsafe fn dib(size: i32) -> Option<(windows::Win32::Graphics::Gdi::HDC, windows::Win32::Graphics::Gdi::HBITMAP, *mut u32)> {
    let dc = unsafe { CreateCompatibleDC(None) };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let Ok(bitmap) = (unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }) else {
        unsafe {
            let _ = DeleteDC(dc);
        }
        return None;
    };
    unsafe {
        SelectObject(dc, bitmap.into());
    }
    Some((dc, bitmap, bits.cast()))
}

/// 글자의 덮임(0~255)을 칸 가운데로 옮겨 돌려준다. `DT_VCENTER` 는 글꼴 칸 높이로 가운데를 잡아 숫자가
/// 아래로 처진다 — 실제로 칠해진 상자로 다시 맞춘다.
fn text_coverage(label: &str, size: i32) -> Option<Vec<u8>> {
    let n = size as usize;
    let mut raw = vec![0u8; n * n];
    unsafe {
        let (dc, bitmap, bits) = dib(size)?;
        let height = (size as f32 * if label.chars().count() > 1 { 0.62 } else { 0.74 }).round() as i32;
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            &HSTRING::from("Segoe UI"),
        );
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(0x00FF_FFFF));
        SetBkMode(dc, TRANSPARENT);
        let mut rect = RECT { left: 0, top: 0, right: size, bottom: size };
        let mut text: Vec<u16> = label.encode_utf16().collect();
        DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        let _ = GdiFlush();
        let px = std::slice::from_raw_parts(bits, n * n);
        for (i, p) in px.iter().enumerate() {
            raw[i] = ((p >> 8) & 0xFF) as u8;
        }
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
    }
    Some(recenter(&raw, n))
}

fn recenter(raw: &[u8], n: usize) -> Vec<u8> {
    let lit: Vec<(usize, usize)> = (0..n * n).filter(|&i| raw[i] > 24).map(|i| (i % n, i / n)).collect();
    let (Some(x0), Some(x1)) = (lit.iter().map(|p| p.0).min(), lit.iter().map(|p| p.0).max()) else {
        return raw.to_vec();
    };
    let (y0, y1) = (lit.iter().map(|p| p.1).min().unwrap_or(0), lit.iter().map(|p| p.1).max().unwrap_or(0));
    let dx = (n as i32 - (x1 - x0 + 1) as i32) / 2 - x0 as i32;
    let dy = (n as i32 - (y1 - y0 + 1) as i32) / 2 - y0 as i32;
    let mut out = vec![0u8; n * n];
    for y in 0..n as i32 {
        for x in 0..n as i32 {
            let (sx, sy) = (x - dx, y - dy);
            if (0..n as i32).contains(&sx) && (0..n as i32).contains(&sy) {
                out[(y * n as i32 + x) as usize] = raw[(sy * n as i32 + sx) as usize];
            }
        }
    }
    out
}

/// 꽉 찬 원(가장자리 4×4 표본으로 부드럽게)에 흰 글자를 덮임만큼 섞는다. 아이콘 픽셀은 BGRA, 곱하지 않은 알파.
fn compose_badge(n: usize, cover: &[u8], fill: [u8; 4]) -> Vec<u32> {
    let r = n as f32 / 2.0;
    let mut out = vec![0u32; n * n];
    for y in 0..n {
        for x in 0..n {
            let mut hit = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0 - r;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0 - r;
                    if fx * fx + fy * fy <= r * r {
                        hit += 1;
                    }
                }
            }
            if hit == 0 {
                continue;
            }
            let a = (hit * 255 / 16) as u32;
            let t = cover[y * n + x] as u32;
            let mix = |c: u8| (c as u32 * (255 - t) + 255 * t) / 255;
            out[y * n + x] = (a << 24) | (mix(fill[0]) << 16) | (mix(fill[1]) << 8) | mix(fill[2]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_text_cannot_break_out_of_the_document() {
        let xml = toast_xml("유우카 <끝>", "a & b \"c\"\u{1}", None, "kasaterm-notify-1|%3|");
        assert!(xml.contains("<text>유우카 &lt;끝&gt;</text>"));
        assert!(xml.contains("<text>a &amp; b &quot;c&quot; </text>"));
        assert!(xml.contains(r#"launch="kasaterm-notify-1|%3|""#));
        assert!(!xml.contains("<image"));
    }

    #[test]
    fn image_paths_become_file_uris() {
        let uri = file_uri(Path::new(r"C:\Users\김 선\AppData\yuuka.png"));
        assert_eq!(uri, "file:///C:/Users/%EA%B9%80%20%EC%84%A0/AppData/yuuka.png");
    }

    #[test]
    fn only_cargo_build_folders_are_quiet() {
        assert!(in_cargo_target(Path::new(r"C:\w\kasaterm\target\debug\kasaterm.exe")));
        assert!(in_cargo_target(Path::new(r"C:\w\target\x86_64-pc-windows-msvc\release\kasaterm.exe")));
        assert!(!in_cargo_target(Path::new(r"C:\Program Files\kasaterm\bin\kasaterm.exe")));
        assert!(!in_cargo_target(Path::new(r"D:\target\kasaterm\bin\kasaterm.exe")));
    }

    #[test]
    fn badge_is_a_round_disc_with_a_white_digit() {
        assert_eq!(badge_label(3), "3");
        assert_eq!(badge_label(12), "9+");
        let n = 16;
        let mut cover = vec![0u8; n * n];
        cover[8 * n + 8] = 255;
        let px = compose_badge(n, &cover, [224, 88, 78, 255]);
        assert_eq!(px[0] >> 24, 0, "corner stays clear");
        assert_eq!(px[4 * n + 8] >> 24, 255, "inside the disc is opaque");
        assert_eq!(px[4 * n + 8] & 0x00FF_FFFF, (224 << 16) | (88 << 8) | 78);
        assert_eq!(px[8 * n + 8] & 0x00FF_FFFF, 0x00FF_FFFF, "text pixel is white");
    }

    #[test]
    fn drawn_text_is_moved_to_the_middle() {
        let n = 8;
        let mut raw = vec![0u8; n * n];
        raw[6 * n + 6] = 255;
        let out = recenter(&raw, n);
        assert_eq!(out.iter().position(|&v| v == 255), Some(3 * n + 3));
    }
}
