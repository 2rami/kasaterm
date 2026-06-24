//! macOS Sparkle 자동 업데이트 FFI.
//!
//! Sparkle 은 objc2 바인딩이 없어 raw class lookup(`macos_open.rs` 와 같은 패턴)으로
//! 부른다. `build.rs` 링크 대신 런타임 `dlopen` 을 쓴다 — dev 빌드(`cargo run`)엔
//! Sparkle.framework 가 없어 dlopen 이 실패하고 graceful no-op(업데이터 없이 정상
//! 기동), `.app` 빌드(Contents/Frameworks/Sparkle.framework)에서만 활성화된다.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;

/// Sparkle.framework 를 dlopen 해 `SPUStandardUpdaterController` 를 만들고 백그라운드
/// 자동 업데이트 체크를 시작한다. 반환된 controller 는 App 에 **보관해야 한다** —
/// 드롭되면 updater 가 정지한다. framework 가 없으면(dev 빌드) `None`.
pub(crate) fn init() -> Option<Retained<AnyObject>> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::NSBundle;

    unsafe {
        // .app/Contents/Frameworks/Sparkle.framework/Versions/B/Sparkle 를 dlopen 해
        // Objective-C 클래스를 런타임에 등록한다. privateFrameworksPath = Contents/Frameworks.
        let fw_dir = NSBundle::mainBundle().privateFrameworksPath()?;
        let dylib = format!("{fw_dir}/Sparkle.framework/Versions/B/Sparkle");
        let c = std::ffi::CString::new(dylib).ok()?;
        if libc::dlopen(c.as_ptr(), libc::RTLD_NOW).is_null() {
            return None; // dev 빌드 — framework 미번들. graceful no-op.
        }

        let cls = AnyClass::get(c"SPUStandardUpdaterController")?;
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        if alloc.is_null() {
            return None;
        }
        // initWithStartingUpdater:YES → startUpdater 가 자동 호출되어 백그라운드 체크 시작.
        // SPUStandardUpdaterController 는 sharedUpdater 가 없다 — 인스턴스를 만들어 보관한다.
        let nil: *mut AnyObject = std::ptr::null_mut();
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithStartingUpdater: Bool::YES,
            updaterDelegate: nil,
            userDriverDelegate: nil,
        ];
        Retained::from_raw(obj) // init 의 +1 retain 을 소유로 가져온다
    }
}

/// "업데이트 확인" 메뉴 → 보관된 controller 에 `checkForUpdates:` 위임(표준 다이얼로그).
pub(crate) fn check_for_updates(controller: &AnyObject) {
    use objc2::msg_send;
    unsafe {
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![controller, checkForUpdates: nil];
    }
}
