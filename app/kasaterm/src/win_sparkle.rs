//! Windows 자동 업데이트 — WinSparkle.dll 을 런타임 `LoadLibrary` 로 얹는다
//! (`macos_sparkle.rs` 의 `dlopen` 패턴 대칭). DLL 이 없으면(dev `cargo run`,
//! 또는 WinSparkle.dll 미배치) graceful no-op — 업데이터 없이 정상 기동한다.
//! MSI 빌드는 exe 옆(`bin\WinSparkle.dll`)에 함께 설치하므로 프로덕션에서만 활성.
//!
//! `#[link]` 정적 링크 대신 런타임 로드라 빌드엔 WinSparkle.lib 가 필요 없다 —
//! DLL 은 순전히 런타임 의존이라 `cargo build` 는 DLL 없이도 통과한다.

use std::ffi::c_char;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// Windows 전용 appcast 피드(mac 은 appcast.xml 을 따로 쓴다). EdDSA 공개키는 mac
// Sparkle 과 **공유** — `.msi` 를 mac `sign_update`(Keychain 개인키)로 서명하면
// WinSparkle 이 이 공개키로 검증한다(서명 알고리즘 호환). build-app.sh 의
// SUPublicEDKey 와 동일해야 한다.
const APPCAST_URL: &str = "https://2rami.github.io/kasaterm/appcast-win.xml";
const ED_PUBLIC_KEY: &str = "E4tFAb2UND+0QhgTSv2pFYKIC3ReT/dLia20KHfZxKw=";

static STARTED: AtomicBool = AtomicBool::new(false);

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

/// WinSparkle 백그라운드 자동 업데이트를 시작한다(프로세스당 한 번). DLL 이 없으면
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
        let set_interval = load!("win_sparkle_set_update_check_interval", CSetInt);
        let start = load!("win_sparkle_init", CVoid);

        // 모든 set_* 는 win_sparkle_init() 전에 호출해야 한다(winsparkle.h 규약).
        let url = utf8z(APPCAST_URL);
        set_appcast_url(url.as_ptr() as *const c_char);
        let key = utf8z(ED_PUBLIC_KEY);
        set_eddsa_key(key.as_ptr() as *const c_char);
        let company = utf16z("momewomo");
        let app = utf16z("kasaterm");
        let version = utf16z(env!("CARGO_PKG_VERSION"));
        set_app_details(company.as_ptr(), app.as_ptr(), version.as_ptr());
        set_auto_check(1);
        set_interval(86_400); // 하루(최소 3600초). mac SUScheduledCheckInterval 과 동일.
        start();
    }
}
