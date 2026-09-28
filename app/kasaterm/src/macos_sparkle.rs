//! macOS Sparkle 자동 업데이트 FFI.
//!
//! Sparkle 은 objc2 바인딩이 없어 raw class lookup(`macos_open.rs` 와 같은 패턴)으로
//! 부른다. `build.rs` 링크 대신 런타임 `dlopen` 을 쓴다 — dev 빌드(`cargo run`)엔
//! Sparkle.framework 가 없어 dlopen 이 실패하고 graceful no-op(업데이터 없이 정상
//! 기동), `.app` 빌드(Contents/Frameworks/Sparkle.framework)에서만 활성화된다.

use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSString, NSUserDefaults};

const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-preview.xml";

#[derive(Default)]
struct UpdateState {
    owner: AtomicBool,
    ready: AtomicBool,
}

fn update_state() -> &'static Arc<UpdateState> {
    static STATE: OnceLock<Arc<UpdateState>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(UpdateState::default()))
}

pub(crate) fn owns_installation() -> bool { update_state().owner.load(Ordering::Acquire) }
pub(crate) fn install_on_quit_ready() -> bool { update_state().ready.load(Ordering::Acquire) }

fn preview_opted_in(settings: &serde_json::Value) -> bool {
    settings["update_channel"].as_str() == Some("preview")
        && settings["automatic_update_on_quit"].as_bool() == Some(true)
}

fn installed_app(exe: &Path, home: &Path, verification: bool) -> bool {
    !verification && (exe == home.join("Applications/kasaterm.app/Contents/MacOS/kasaterm")
        || exe == Path::new("/Applications/kasaterm.app/Contents/MacOS/kasaterm"))
}

fn isolated_environment(has: impl Fn(&str) -> bool) -> bool {
    ["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "KASATERM_WINDOW_FILE",
        "KASATERM_AUTOQUIT_MS", "KASATERM_LITE_ROOT"].iter().any(|key| has(key))
}

fn writable_install_parent(exe: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Some(parent) = exe.ancestors().nth(4) else { return false };
    let Ok(path) = std::ffi::CString::new(parent.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

struct DelegateIvars {
    feed: Retained<NSString>,
    state: Arc<UpdateState>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KasatermQuitUpdateDelegate"]
    #[ivars = DelegateIvars]
    struct QuitUpdateDelegate;

    unsafe impl NSObjectProtocol for QuitUpdateDelegate {}

    impl QuitUpdateDelegate {
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> Retained<NSString> {
            self.ivars().feed.clone()
        }

        #[unsafe(method(updater:willInstallUpdateOnQuit:immediateInstallationBlock:))]
        fn install_on_quit(&self, _updater: &AnyObject, _item: &AnyObject,
            _immediate: &block2::DynBlock<dyn Fn()>) -> Bool {
            // Sparkle still installs on termination when YES is returned;
            // invoking the block would instead terminate the live workspace.
            self.ivars().state.ready.store(true, Ordering::Release);
            Bool::YES
        }

        #[unsafe(method(updaterShouldRelaunchApplication:))]
        fn relaunch(&self, _updater: &AnyObject) -> Bool { Bool::NO }

        #[unsafe(method(supportsGentleScheduledUpdateReminders))]
        fn gentle_reminders(&self) -> Bool { Bool::YES }

        #[unsafe(method(standardUserDriverShouldHandleShowingScheduledUpdate:andInImmediateFocus:))]
        fn show_scheduled(&self, _item: &AnyObject, _immediate_focus: Bool) -> Bool { Bool::NO }

        #[unsafe(method(standardUserDriverWillHandleShowingUpdate:forUpdate:state:))]
        fn scheduled_notice(&self, _standard_shows: Bool, _item: &AnyObject, _state: &AnyObject) {}
    }
);

impl QuitUpdateDelegate {
    fn new(state: Arc<UpdateState>) -> Retained<Self> {
        let delegate = Self::alloc().set_ivars(DelegateIvars {
            feed: NSString::from_str(PREVIEW_FEED), state,
        });
        unsafe { msg_send![super(delegate), init] }
    }
}

pub(crate) struct Updater {
    controller: Retained<AnyObject>,
    // Sparkle only holds weak delegate references.
    _delegate: Option<Retained<QuitUpdateDelegate>>,
    preview: bool,
}

/// Process-only overrides leave stable preferences intact after opting out.
unsafe fn configure_automatic_session(defaults: &NSUserDefaults) -> Option<()> {
    let domain = NSString::from_str("NSArgumentDomain");
    let existing: *mut AnyObject = msg_send![defaults, volatileDomainForName: &*domain];
    let copy: *mut AnyObject = msg_send![existing, mutableCopy];
    let values = Retained::from_raw(copy)?;
    let number = AnyClass::get(c"NSNumber")?;
    let yes: *mut AnyObject = msg_send![number, numberWithBool: Bool::YES];
    for key in ["SUEnableAutomaticChecks", "SUAutomaticallyUpdate"] {
        let key = NSString::from_str(key);
        let _: () = msg_send![&*values, setObject: yes, forKey: &*key];
    }
    let interval: *mut AnyObject = msg_send![number, numberWithDouble: 3600.0_f64];
    let key = NSString::from_str("SUScheduledCheckInterval");
    let _: () = msg_send![&*values, setObject: interval, forKey: &*key];
    let _: () = msg_send![defaults, setVolatileDomain: &*values, forName: &*domain];
    Some(())
}

/// Sparkle.framework 를 dlopen 해 `SPUStandardUpdaterController` 를 만들고 백그라운드
/// 자동 업데이트 체크를 시작한다. 반환된 controller 는 App 에 **보관해야 한다** —
/// 드롭되면 updater 가 정지한다. framework 가 없으면(dev 빌드) `None`.
pub(crate) fn init() -> Option<Updater> {
    let exe = std::env::current_exe().ok()?;
    let home = kasa_socket::home_dir()?;
    let isolated = crate::verification_run() || isolated_environment(|key| std::env::var_os(key).is_some());
    if !installed_app(&exe, &home, isolated) { return None; }
    let preview = preview_opted_in(&crate::socket::read_settings());
    if preview && !writable_install_parent(&exe) { return None; }
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
        let nil: *mut AnyObject = std::ptr::null_mut();
        let delegate = preview.then(|| QuitUpdateDelegate::new(update_state().clone()));
        let delegate_ptr = delegate.as_ref().map_or(nil, |value| (&**value as *const QuitUpdateDelegate).cast_mut().cast());
        if preview { configure_automatic_session(&NSUserDefaults::standardUserDefaults())?; }
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithStartingUpdater: Bool::from(!preview),
            updaterDelegate: delegate_ptr,
            userDriverDelegate: delegate_ptr,
        ];
        let controller = Retained::from_raw(obj)?;
        if preview {
            let updater: *mut AnyObject = msg_send![&*controller, updater];
            let mut error: *mut NSError = std::ptr::null_mut();
            let started: Bool = msg_send![updater, startUpdater: &mut error];
            if !started.as_bool() { return None; }
            // Selecting a single writer before the first update cycle also
            // covers a local dist build that finishes after Sparkle stages.
            update_state().owner.store(true, Ordering::Release);
            let _: () = msg_send![updater, checkForUpdatesInBackground];
        }
        Some(Updater { controller, _delegate: delegate, preview })
    }
}

/// "업데이트 확인" 메뉴 → 보관된 controller 에 `checkForUpdates:` 위임(표준 다이얼로그).
pub(crate) fn check_for_updates(updater: &Updater) {
    unsafe {
        if updater.preview {
            let instance: *mut AnyObject = msg_send![&*updater.controller, updater];
            let available: Bool = msg_send![instance, canCheckForUpdates];
            if available.as_bool() {
                let _: () = msg_send![instance, checkForUpdatesInBackground];
            }
            return;
        }
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![&*updater.controller, checkForUpdates: nil];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_requires_both_explicit_settings() {
        assert!(preview_opted_in(&serde_json::json!({"update_channel":"preview", "automatic_update_on_quit":true})));
        for settings in [serde_json::json!({}), serde_json::json!({"update_channel":"preview"}),
            serde_json::json!({"update_channel":"stable", "automatic_update_on_quit":true}),
            serde_json::json!({"update_channel":"preview", "automatic_update_on_quit":false})] {
            assert!(!preview_opted_in(&settings));
        }
    }

    #[test]
    fn development_and_verification_instances_never_start_sparkle() {
        let home = Path::new("/Users/test");
        let installed = home.join("Applications/kasaterm.app/Contents/MacOS/kasaterm");
        assert!(installed_app(&installed, home, false));
        assert!(installed_app(Path::new("/Applications/kasaterm.app/Contents/MacOS/kasaterm"), home, false));
        assert!(!installed_app(&installed, home, true));
        assert!(!installed_app(Path::new("/tmp/rig/kasaterm.app/Contents/MacOS/kasaterm"), home, false));
        assert!(!installed_app(Path::new("/repo/target/debug/kasaterm"), home, false));
        assert!(!isolated_environment(|_| false));
        for key in ["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "KASATERM_WINDOW_FILE", "KASATERM_AUTOQUIT_MS", "KASATERM_LITE_ROOT"] {
            assert!(isolated_environment(|candidate| candidate == key));
        }
    }

    #[test]
    fn quit_delegate_never_invokes_immediate_install_or_relaunch() {
        let state = Arc::new(UpdateState::default());
        let delegate = QuitUpdateDelegate::new(state.clone());
        let dummy = NSObject::new();
        let called = Arc::new(AtomicBool::new(false));
        let observed = called.clone();
        let immediate = block2::RcBlock::new(move || { observed.store(true, Ordering::Release); });
        let reply: Bool = unsafe { msg_send![&*delegate, updater: &*dummy,
            willInstallUpdateOnQuit: &*dummy, immediateInstallationBlock: &*immediate] };
        assert!(reply.as_bool());
        assert!(state.ready.load(Ordering::Acquire));
        assert!(!called.load(Ordering::Acquire));
        let relaunch: Bool = unsafe { msg_send![&*delegate, updaterShouldRelaunchApplication: &*dummy] };
        let scheduled: Bool = unsafe { msg_send![&*delegate,
            standardUserDriverShouldHandleShowingScheduledUpdate: &*dummy, andInImmediateFocus: Bool::YES] };
        let feed: Retained<NSString> = unsafe { msg_send![&*delegate, feedURLStringForUpdater: &*dummy] };
        assert!(!relaunch.as_bool());
        assert!(!scheduled.as_bool());
        assert_eq!(feed.to_string(), PREVIEW_FEED);
    }

    #[test]
    fn automatic_options_do_not_persist_into_stable_preferences() {
        let suite = NSString::from_str(&format!("com.kasa.kasaterm.preview-test.{}", uuid::Uuid::new_v4()));
        let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)).unwrap();
        let before = defaults.persistentDomainForName(&suite);
        unsafe { configure_automatic_session(&defaults).unwrap(); }
        assert!(defaults.boolForKey(&NSString::from_str("SUEnableAutomaticChecks")));
        assert!(defaults.boolForKey(&NSString::from_str("SUAutomaticallyUpdate")));
        assert_eq!(defaults.doubleForKey(&NSString::from_str("SUScheduledCheckInterval")), 3600.0);
        assert_eq!(defaults.persistentDomainForName(&suite), before);
        defaults.removeVolatileDomainForName(&NSString::from_str("NSArgumentDomain"));
    }
}
