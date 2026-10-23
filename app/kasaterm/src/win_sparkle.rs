//! Windows 자동 업데이트 — WinSparkle.dll 을 런타임 `LoadLibrary` 로 얹는다
//! (`macos_sparkle.rs` 의 `dlopen` 패턴 대칭). DLL 이 없으면(dev `cargo run`,
//! 또는 WinSparkle.dll 미배치) graceful no-op — 업데이터 없이 정상 기동한다.
//! MSI 빌드는 exe 옆(`bin\WinSparkle.dll`)에 함께 설치하므로 프로덕션에서만 활성.
//!
//! `#[link]` 정적 링크 대신 런타임 로드라 빌드엔 WinSparkle.lib 가 필요 없다 —
//! DLL 은 순전히 런타임 의존이라 `cargo build` 는 DLL 없이도 통과한다.
//!
//! ## "업데이트 있음" UI 는 kasaterm 토스트로
//!
//! WinSparkle 은 자체 체크가 업데이트를 찾으면 Win32 구식 다이얼로그를 무조건
//! 띄우고, 이를 끄거나 꾸밀 API 가 없다. 그래서 **체크는 우리가, 설치만
//! WinSparkle 에게**: 자동 체크를 0 으로 끄고 appcast 를 직접 받아(curl, UI 없음)
//! 버전을 비교한 뒤, 새 버전이면 kasaterm 새 판 알림(`update_notice.rs`, [업데이트][닫기]
//! 칩)으로 알린다. [업데이트] 클릭 시에만
//! `win_sparkle_check_update_with_ui_and_install` 로 위임 — "업데이트 있음"
//! 창을 건너뛰고 다운로드 진행바 → EdDSA 검증 → MSI 실행까지 WinSparkle 의
//! 검증·설치 머신을 그대로 쓴다.
//!
//! 확인 주기는 맥과 같다 — 켠 지 10초 뒤, 그 뒤 안정판은 하루, preview 참여 기기는 한 시간마다. 참여 판정도
//! 맥과 같은 두 설정(`version::preview_opted_in`)이고, 참여 기기는 피드만 preview 로 바뀐다.
//!
//! MSI 는 앱이 떠 있으면 「Files in Use」를 묻고 사람이 앱을 꺼야 했다. 그래서 WinSparkle 이 설치기를 띄우기
//! 직전(can-shutdown)에 끊길 일을 다시 재고, 띄운 직후(shutdown-request) 앱이 스스로 정상 종료(`exiting` — 세션
//! 저장)한다. 알림의 [그래도 업데이트]에서 이미 받아들인 끊김과 같으면 다시 묻지 않는다.

use std::sync::Mutex;
#[cfg(any(windows, test))]
use std::time::Duration;

#[cfg(any(windows, test))]
const STABLE_FEED: &str = "https://2rami.github.io/kasaterm/appcast-win.xml";
#[cfg(any(windows, test))]
const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-win-preview.xml";
/// 켠 뒤 첫 확인까지 — 창이 서고 세션이 돌아온 뒤에 알림이 서게 한다(맥 `FIRST_PROBE`).
#[cfg(any(windows, test))]
const FIRST_PROBE: Duration = Duration::from_secs(10);

/// 이 기기가 받는 판 — 빠른 업데이트(preview)에 참여했나.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Stable,
    Preview,
}

#[cfg(any(windows, test))]
impl Channel {
    pub(crate) fn of(settings: &serde_json::Value) -> Self {
        if crate::version::preview_opted_in(settings) { Self::Preview } else { Self::Stable }
    }

    /// 이 채널의 Windows 피드.
    pub(crate) fn feed(self) -> &'static str {
        match self {
            Self::Stable => STABLE_FEED,
            Self::Preview => PREVIEW_FEED,
        }
    }

    /// 다음 확인까지 — 맥 안정판(Info.plist `SUScheduledCheckInterval`)과 preview(`PROBE_EVERY`) 그대로.
    fn probe_every(self) -> Duration {
        Duration::from_secs(match self {
            Self::Stable => 86_400,
            Self::Preview => 3600,
        })
    }
}

/// 알림의 [그래도 업데이트]에서 사람이 받아들인 끊김. 설치 직전 끊김이 이것과 같으면 다시 묻지 않는다.
static ACCEPTED: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn accept_interrupts(what: &str) {
    if let Ok(mut accepted) = ACCEPTED.lock() {
        *accepted = Some(what.to_string());
    }
}

/// 설치기를 띄워도 되나. `asked` 는 GUI 스레드의 답 — `None` 이면 앱이 답하지 못한 것이고(멈춤) 그땐 끄지 않는다.
/// 끊길 일이 있으면 이미 받아들인 것과 같을 때만 그냥 가고, 아니면 사람에게 묻는다.
#[cfg(any(windows, test))]
fn may_quit(asked: Option<Option<String>>, accepted: Option<&str>, confirm: impl FnOnce(&str) -> bool) -> bool {
    match asked {
        None => false,
        Some(None) => true,
        Some(Some(what)) if accepted == Some(what.as_str()) => true,
        Some(Some(what)) => confirm(&what),
    }
}

/// 체커 스레드가 찾은 새 버전. GUI(about_to_wait)가 take 해 새 판 알림을 세운다.
/// 주기 확인이 같은 판을 다시 찾아도 알림 쪽(`offer_update`)이 서 있는 알림·닫은 판을 거른다.
static FOUND: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn take_found() -> Option<String> {
    // 테스트 훅: KASATERM_FAKE_UPDATE=<버전> 을 첫 호출에서 1회 소비 — 업데이터 없이
    // (mac dev 빌드 포함) 새 판 알림 UI 를 검증한다(testkit env 하네스 결).
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static FAKE_TAKEN: AtomicBool = AtomicBool::new(false);
        if !FAKE_TAKEN.swap(true, Ordering::SeqCst) {
            if let Ok(v) = std::env::var("KASATERM_FAKE_UPDATE") {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    FOUND.lock().ok()?.take()
}

/// appcast XML 에서 최신 항목의 `sparkle:version` 을 뽑는다. CI 가 최신 1건만
/// 쓰는 단순 피드(docs/appcast-win.xml)라 첫 매치가 곧 최신이다.
/// 실피드(release.sh/release.yml)는 `<sparkle:version>x</sparkle:version>`
/// 엘리먼트형 — 속성형(`sparkle:version="x"`)은 Sparkle 호환 폴백.
pub(crate) fn parse_appcast_version(xml: &str) -> Option<String> {
    const ELEM: &str = "<sparkle:version>";
    if let Some(i) = xml.find(ELEM) {
        let rest = &xml[i + ELEM.len()..];
        let v = rest[..rest.find('<')?].trim();
        return (!v.is_empty()).then(|| v.to_string());
    }
    const ATTR: &str = "sparkle:version=\"";
    let i = xml.find(ATTR)? + ATTR.len();
    let rest = &xml[i..];
    let v = &rest[..rest.find('"')?];
    (!v.is_empty()).then(|| v.to_string())
}

/// a 가 b 보다 새 버전인가 — 세그먼트별 숫자 비교("0.1.10" > "0.1.9").
/// 숫자가 아닌 세그먼트가 섞이면 보수적으로 false(오판 업데이트 알림 방지).
pub(crate) fn version_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// 이 실행에 WinSparkle 을 얹어도 되나. DLL 은 init 만으로 HKCU 에 제 설정을 쓰고 체커는 설치본
/// 피드를 보니, 검증 리그·격리 env·라이트 실행은 DLL 을 싣기 전에 거른다(맥 Sparkle 과 같은 판정).
#[cfg(any(windows, test))]
fn may_start(verification: bool, lite: bool, has: impl Fn(&str) -> bool) -> bool {
    !verification && !lite && !crate::version::isolated_environment(has)
}

/// 새 판 알림 [업데이트] → WinSparkle 다운로드·설치로 위임. mac 에선 no-op
/// (부르는 쪽이 `available()` 로 먼저 거르지만 심볼은 공용으로 둔다).
#[cfg(not(windows))]
pub(crate) fn install() {}

/// 이 실행에 WinSparkle 이 얹혔나 — MSI 로 설치한 판만 DLL 이 곁에 있다.
#[cfg(not(windows))]
pub(crate) fn available() -> bool {
    false
}

#[cfg(windows)]
pub(crate) use ffi::{available, init, install};

#[cfg(windows)]
mod ffi {
    use super::{parse_appcast_version, version_newer, ACCEPTED, FIRST_PROBE, FOUND};
    use crate::UserEvent;
    use std::ffi::c_char;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO,
    };

    // EdDSA 공개키는 mac Sparkle 과 **공유** — `.msi` 를 mac `sign_update`(Keychain 개인키)로 서명하면
    // WinSparkle 이 이 공개키로 검증한다(서명 알고리즘 호환). build-app.sh 의 SUPublicEDKey 와 동일해야 한다.
    const ED_PUBLIC_KEY: &str = "E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=";
    /// 설치 직전 GUI 스레드에 끊길 일을 묻고 기다리는 한도. 넘기면 앱이 멈춘 것이라 끄지 않는다.
    const ASK_TIMEOUT: Duration = Duration::from_secs(5);

    static STARTED: AtomicBool = AtomicBool::new(false);
    // win_sparkle_check_update_with_ui_and_install 함수 포인터 — init 성공 시
    // 채워지고 install() 이 호출한다. 0 이면(DLL 없음) install 은 no-op.
    static INSTALL_FN: AtomicUsize = AtomicUsize::new(0);
    /// WinSparkle 콜백은 주 스레드가 아닌 곳에서 온다 — GUI 에는 이벤트로만 닿는다.
    static PROXY: OnceLock<Mutex<winit::event_loop::EventLoopProxy<UserEvent>>> = OnceLock::new();

    // WinSparkle C API (__cdecl). x64 는 호출규약이 하나라 extern "C" 로 매핑된다.
    // URL/key 는 char*(UTF-8), app_details 는 wchar_t*(UTF-16) — winsparkle.h 참고.
    type CSetStr = unsafe extern "C" fn(*const c_char);
    type CSetEdKey = unsafe extern "C" fn(*const c_char) -> i32;
    type CSetDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
    type CSetInt = unsafe extern "C" fn(i32);
    type CVoid = unsafe extern "C" fn();
    type CSetCanShutdown = unsafe extern "C" fn(Option<unsafe extern "C" fn() -> i32>);
    type CSetShutdownRequest = unsafe extern "C" fn(Option<unsafe extern "C" fn()>);

    fn utf16z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn utf8z(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn send(event: UserEvent) -> bool {
        PROXY.get().and_then(|p| p.lock().ok()).is_some_and(|p| p.send_event(event).is_ok())
    }

    /// WinSparkle 이 설치기를 띄우기 직전. 끊길 일은 GUI 스레드만 알아 묻고 기다린다(이 콜백은 주 스레드가 아니다).
    unsafe extern "C" fn can_shutdown() -> i32 {
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = send(UserEvent::UpdaterMayQuit(tx)).then(|| rx.recv_timeout(ASK_TIMEOUT).ok()).flatten();
        let accepted = ACCEPTED.lock().ok().and_then(|a| a.clone());
        super::may_quit(asked, accepted.as_deref(), confirm) as i32
    }

    /// 설치기를 띄운 직후 — 앱이 정상 종료(`exiting`)로 세션을 저장하고 꺼져 MSI 가 파일을 갈아 끼운다.
    unsafe extern "C" fn shutdown_request() {
        send(UserEvent::UpdaterQuit);
    }

    /// 맥의 「지금 다시 켜면 끊겨요」 시트와 같은 자리·같은 기본값(나중에 = 아니요).
    fn confirm(what: &str) -> bool {
        let text = utf16z(&format!("지금 다시 켜면 끊겨요 · {what}\n\n그래도 업데이트할까요?"));
        let caption = utf16z("kasaterm 업데이트");
        let flags = MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND;
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), flags) == IDYES }
    }

    /// WinSparkle 설치 머신을 초기화하고(자동 체크 OFF — 다이얼로그 원천 차단)
    /// 자체 appcast 버전 체커 스레드를 띄운다(프로세스당 한 번). DLL 이 없으면
    /// 조용히 no-op. `resumed()` 가 사실상 1회라 STARTED 가드는 이중 안전장치.
    pub(crate) fn init(proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
        if !super::may_start(crate::verification_run(), crate::lite_mode(), |key| std::env::var_os(key).is_some()) {
            return;
        }
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let channel = super::Channel::of(&crate::socket::read_settings());
        let feed = channel.feed();
        unsafe {
            let dll = utf16z("WinSparkle.dll");
            let lib = LoadLibraryW(dll.as_ptr());
            if lib.is_null() {
                return; // dev / DLL 미배치 → 업데이터 없이 계속.
            }
            // 필요한 심볼을 GetProcAddress 로 모은다. 하나라도 없으면(DLL 버전 불일치) 포기.
            macro_rules! load {
                ($name:literal, $ty:ty) => {{
                    let n = utf8z($name);
                    match GetProcAddress(lib, n.as_ptr()) {
                        Some(p) => std::mem::transmute::<_, $ty>(p),
                        None => return,
                    }
                }};
            }
            let set_appcast_url = load!("win_sparkle_set_appcast_url", CSetStr);
            let set_eddsa_key = load!("win_sparkle_set_eddsa_public_key", CSetEdKey);
            let set_app_details = load!("win_sparkle_set_app_details", CSetDetails);
            let set_auto_check = load!("win_sparkle_set_automatic_check_for_updates", CSetInt);
            let set_can_shutdown = load!("win_sparkle_set_can_shutdown_callback", CSetCanShutdown);
            let set_shutdown_request = load!("win_sparkle_set_shutdown_request_callback", CSetShutdownRequest);
            let start = load!("win_sparkle_init", CVoid);
            let install_fn = load!("win_sparkle_check_update_with_ui_and_install", CVoid);

            // 모든 set_* 는 win_sparkle_init() 전에 호출해야 한다(winsparkle.h 규약).
            let url = utf8z(feed);
            set_appcast_url(url.as_ptr() as *const c_char);
            let key = utf8z(ED_PUBLIC_KEY);
            set_eddsa_key(key.as_ptr() as *const c_char);
            let company = utf16z("momewomo");
            let app = utf16z("kasaterm");
            let version = utf16z(env!("CARGO_PKG_VERSION"));
            set_app_details(company.as_ptr(), app.as_ptr(), version.as_ptr());
            // 자동 체크 OFF — WinSparkle 스스로 체크하게 두면 업데이트 발견 시
            // 구식 "업데이트 있음" 다이얼로그를 무조건 띄운다(끄는 API 없음).
            // 체크는 아래 체커 스레드가 UI 없이 대신한다.
            set_auto_check(0);
            let _ = PROXY.set(Mutex::new(proxy));
            set_can_shutdown(Some(can_shutdown));
            set_shutdown_request(Some(shutdown_request));
            start();
            INSTALL_FN.store(install_fn as usize, Ordering::SeqCst);
        }
        // appcast 를 직접 받아 버전 비교(UI 없음). 새 버전이면 FOUND 에 기록 → GUI(about_to_wait)가 새 판 알림으로 알린다.
        std::thread::spawn(move || {
            std::thread::sleep(FIRST_PROBE);
            loop {
                if let Some(latest) = fetch_appcast(feed).and_then(|xml| parse_appcast_version(&xml)) {
                    if version_newer(&latest, env!("CARGO_PKG_VERSION")) {
                        if let Ok(mut f) = FOUND.lock() {
                            *f = Some(latest);
                        }
                    }
                }
                std::thread::sleep(channel.probe_every());
            }
        });
    }

    /// appcast XML fetch — Windows 10 1803+ 내장 curl.exe 사용(HTTP 클라이언트
    /// 의존성 0). GUI 앱에서 콘솔 창이 번쩍이지 않게 CREATE_NO_WINDOW.
    fn fetch_appcast(feed: &str) -> Option<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("curl.exe")
            .args(["-fsSL", "--max-time", "15", feed])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub(crate) fn available() -> bool {
        INSTALL_FN.load(Ordering::SeqCst) != 0
    }

    /// 새 판 알림 [업데이트] → WinSparkle 에 다운로드·설치 위임. "업데이트 있음"
    /// 창은 건너뛰고 진행바부터 시작한다(재체크·EdDSA 검증·MSI 실행 포함).
    pub(crate) fn install() {
        let p = INSTALL_FN.load(Ordering::SeqCst);
        if p != 0 {
            unsafe { std::mem::transmute::<usize, CVoid>(p)() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_from_real_feed_element_form() {
        // release.sh/release.yml 이 실제로 뿜는 형식 — 0.1.11 까지 이 형식을
        // 속성형 파서가 못 읽어 업데이트 토스트가 영영 안 떴다(실기 확인).
        let xml = r#"<item><title>0.1.12</title>
            <sparkle:version>0.1.12</sparkle:version>
            <enclosure url="https://x/kasaterm-0.1.12-x86_64.msi"
                sparkle:os="windows" length="123"/></item>"#;
        assert_eq!(parse_appcast_version(xml).as_deref(), Some("0.1.12"));
        assert_eq!(parse_appcast_version("<rss></rss>"), None);
    }

    #[test]
    fn parses_version_from_appcast_enclosure_attr_fallback() {
        let xml = r#"<item><enclosure url="https://x/kasaterm-0.1.9-x86_64.msi"
            sparkle:version="0.1.9" sparkle:os="windows" length="123"/></item>"#;
        assert_eq!(parse_appcast_version(xml).as_deref(), Some("0.1.9"));
    }

    #[test]
    fn version_compare_is_numeric_per_segment() {
        assert!(version_newer("0.1.10", "0.1.9")); // 문자열 비교였다면 false
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(!version_newer("0.1.9", "0.1.9"));
        assert!(!version_newer("0.1.8", "0.1.9"));
        assert!(!version_newer("0.2.0-beta", "0.1.9")); // 비숫자 → 보수적 false
    }

    #[test]
    fn preview_devices_watch_their_own_feed_hourly_and_others_daily() {
        assert_eq!(Channel::Stable.feed(), "https://2rami.github.io/kasaterm/appcast-win.xml");
        assert_eq!(Channel::Preview.feed(), "https://2rami.github.io/kasaterm/appcast-win-preview.xml");
        assert_eq!((Channel::Preview.probe_every(), Channel::Stable.probe_every()),
                   (Duration::from_secs(3600), Duration::from_secs(86_400)));
        assert_eq!(Channel::of(&serde_json::json!({"update_channel":"preview", "automatic_update_on_quit":true})), Channel::Preview);
        assert_eq!(Channel::of(&serde_json::json!({"update_channel":"preview"})), Channel::Stable);
        // 릴리스 도구(tools/release)는 이 글자로 「이 판의 Windows 앱이 preview 피드를 안다」고 판정한다.
        assert!(include_str!("win_sparkle.rs").contains("appcast-win-preview.xml"));
    }

    #[test]
    fn the_installer_waits_for_a_yes_unless_the_same_cut_was_already_accepted() {
        let never = |_: &str| -> bool { panic!("이미 받아들였거나 끊길 일이 없으면 묻지 않는다") };
        assert!(may_quit(Some(None), None, never), "끊길 일이 없으면 바로 설치");
        assert!(may_quit(Some(Some("일하는 창: 유즈".into())), Some("일하는 창: 유즈"), never), "알림에서 받아들인 그 끊김");
        assert!(!may_quit(None, None, never), "앱이 답하지 못하면 끄지 않는다");
        let asked = std::cell::Cell::new(0);
        let say = |answer: bool| {
            let asked = &asked;
            move |what: &str| { assert_eq!(what, "일하는 창: 유즈 외 1개"); asked.set(asked.get() + 1); answer }
        };
        let grown = || Some(Some("일하는 창: 유즈 외 1개".to_string()));
        assert!(!may_quit(grown(), Some("일하는 창: 유즈"), say(false)), "그 뒤 늘어난 끊김은 다시 묻고, 아니요면 미룬다");
        assert!(may_quit(grown(), None, say(true)));
        assert_eq!(asked.get(), 2);
    }

    #[test]
    fn winsparkle_stays_off_in_verification_isolated_and_lite_runs() {
        assert!(may_start(false, false, |_| false));
        assert!(!may_start(true, false, |_| false));
        assert!(!may_start(false, true, |_| false));
        assert!(!may_start(false, false, |key| key == "KASATERM_SETTINGS_FILE"));
    }

    /// 판정이 DLL 적재·STARTED 보다 앞에 있어야 격리 실행이 HKCU 를 한 번도 안 건드린다.
    #[test]
    fn init_refuses_isolated_runs_before_touching_the_dll() {
        let src = include_str!("win_sparkle.rs");
        let init = &src[src.find("pub(crate) fn init()").unwrap()..];
        let guard = init.find("may_start(").unwrap();
        assert!(guard < init.find("STARTED.swap").unwrap());
        assert!(guard < init.find("LoadLibraryW(").unwrap());
    }
}
