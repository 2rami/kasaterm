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
//! 버전을 비교한 뒤, 새 버전이면 kasaterm 토스트([설치][나중에] 칩, 승인 토스트
//! 배관 재사용)로 알린다. [설치] 클릭 시에만
//! `win_sparkle_check_update_with_ui_and_install` 로 위임 — "업데이트 있음"
//! 창을 건너뛰고 다운로드 진행바 → EdDSA 검증 → MSI 실행까지 WinSparkle 의
//! 검증·설치 머신을 그대로 쓴다.

use std::sync::Mutex;

/// 업데이트 토스트의 `collab.toast_action` 센티널 — 승인 토스트 배관(sticky·
/// 칩·클릭 라우팅)을 재사용하되 pane id("%N" 형식)와 절대 충돌하지 않는 값.
/// handler.rs(칩 클릭·본문 클릭)와 render.rs(칩 라벨)가 이 값으로 분기한다.
pub(crate) const UPDATE_TOAST_ACTION: &str = "__kasaterm_update__";

/// 체커 스레드가 찾은 새 버전. GUI(about_to_wait)가 take 해 토스트를 무장한다.
/// take 후엔 이번 실행에선 다시 안 뜬다(다음 실행 때 재체크).
static FOUND: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn take_found() -> Option<String> {
    // 테스트 훅: KASATERM_FAKE_UPDATE=<버전> 을 첫 호출에서 1회 소비 — Windows
    // 실기 없이(mac 포함) 업데이트 토스트 UI 를 검증한다(testkit env 하네스 결).
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
#[cfg_attr(not(windows), allow(dead_code))] // 비 Windows 에선 테스트만 사용
fn parse_appcast_version(xml: &str) -> Option<String> {
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
#[cfg_attr(not(windows), allow(dead_code))] // 비 Windows 에선 테스트만 사용
fn version_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u64>> { s.split('.').map(|p| p.parse().ok()).collect() };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

/// 토스트 [설치] 칩 → WinSparkle 다운로드·설치로 위임. mac 에선 no-op
/// (센티널 토스트 자체가 Windows 에서만 무장되지만 심볼은 공용으로 둔다).
#[cfg(not(windows))]
pub(crate) fn install() {}

#[cfg(windows)]
pub(crate) use ffi::{init, install};

#[cfg(windows)]
mod ffi {
    use super::{parse_appcast_version, version_newer, FOUND};
    use std::ffi::c_char;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

    // Windows 전용 appcast 피드(mac 은 appcast.xml 을 따로 쓴다). EdDSA 공개키는 mac
    // Sparkle 과 **공유** — `.msi` 를 mac `sign_update`(Keychain 개인키)로 서명하면
    // WinSparkle 이 이 공개키로 검증한다(서명 알고리즘 호환). build-app.sh 의
    // SUPublicEDKey 와 동일해야 한다.
    const APPCAST_URL: &str = "https://2rami.github.io/kasaterm/appcast-win.xml";
    const ED_PUBLIC_KEY: &str = "E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=";

    static STARTED: AtomicBool = AtomicBool::new(false);
    // win_sparkle_check_update_with_ui_and_install 함수 포인터 — init 성공 시
    // 채워지고 install() 이 호출한다. 0 이면(DLL 없음) install 은 no-op.
    static INSTALL_FN: AtomicUsize = AtomicUsize::new(0);

    // WinSparkle C API (__cdecl). x64 는 호출규약이 하나라 extern "C" 로 매핑된다.
    // URL/key 는 char*(UTF-8), app_details 는 wchar_t*(UTF-16) — winsparkle.h 참고.
    type CSetStr = unsafe extern "C" fn(*const c_char);
    type CSetEdKey = unsafe extern "C" fn(*const c_char) -> i32;
    type CSetDetails = unsafe extern "C" fn(*const u16, *const u16, *const u16);
    type CSetInt = unsafe extern "C" fn(i32);
    type CVoid = unsafe extern "C" fn();

    fn utf16z(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn utf8z(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    /// WinSparkle 설치 머신을 초기화하고(자동 체크 OFF — 다이얼로그 원천 차단)
    /// 자체 appcast 버전 체커 스레드를 띄운다(프로세스당 한 번). DLL 이 없으면
    /// 조용히 no-op. `resumed()` 가 사실상 1회라 STARTED 가드는 이중 안전장치.
    pub(crate) fn init() {
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
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
            let start = load!("win_sparkle_init", CVoid);
            let install_fn = load!("win_sparkle_check_update_with_ui_and_install", CVoid);

            // 모든 set_* 는 win_sparkle_init() 전에 호출해야 한다(winsparkle.h 규약).
            let url = utf8z(APPCAST_URL);
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
            start();
            INSTALL_FN.store(install_fn as usize, Ordering::SeqCst);
        }
        // 시작 몇 초 뒤 appcast 를 직접 받아 버전 비교(UI 없음). 새 버전이면
        // FOUND 에 기록 → GUI(about_to_wait)가 kasaterm 토스트로 알린다.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(6));
            let Some(xml) = fetch_appcast() else { return };
            let Some(latest) = parse_appcast_version(&xml) else { return };
            if version_newer(&latest, env!("CARGO_PKG_VERSION")) {
                if let Ok(mut f) = FOUND.lock() {
                    *f = Some(latest);
                }
            }
        });
    }

    /// appcast XML fetch — Windows 10 1803+ 내장 curl.exe 사용(HTTP 클라이언트
    /// 의존성 0). GUI 앱에서 콘솔 창이 번쩍이지 않게 CREATE_NO_WINDOW.
    fn fetch_appcast() -> Option<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("curl.exe")
            .args(["-fsSL", "--max-time", "15", APPCAST_URL])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 토스트 [설치] 칩 → WinSparkle 에 다운로드·설치 위임. "업데이트 있음"
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
}
