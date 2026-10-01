//! macOS Sparkle 자동 업데이트 FFI.
//!
//! Sparkle 은 objc2 바인딩이 없어 raw class lookup(`macos_open.rs` 와 같은 패턴)으로
//! 부른다. `build.rs` 링크 대신 런타임 `dlopen` 을 쓴다 — dev 빌드(`cargo run`)엔
//! Sparkle.framework 가 없어 dlopen 이 실패하고 graceful no-op(업데이터 없이 정상
//! 기동), `.app` 빌드(Contents/Frameworks/Sparkle.framework)에서만 활성화된다.
//!
//! preview 기기는 Sparkle 이 판을 **찾기만** 하고, 받기·설치는 사람이 새 판 알림의 [업데이트] 를
//! 눌렀을 때만 한다(`update_notice.rs`). 예전에는 몰래 받아 두었다가 정상 종료 때 설치했는데,
//! 언제 판이 바뀌는지 사람이 모른 채 껐다 켤 때마다 달라져 있었다(2026-10-01). Sparkle 은 한 번
//! 받은 판을 종료 때 반드시 설치하므로, 누르기 전에는 받지 않는 것이 유일한 막음이다.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSString, NSUserDefaults};

const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-preview.xml";
/// 켠 뒤 첫 확인까지. 창이 서고 세션이 돌아온 뒤에 알림이 서게 한다.
const FIRST_PROBE: Duration = Duration::from_secs(10);
const PROBE_EVERY: Duration = Duration::from_secs(3600);
/// `SUErrors.h` 의 `SUNoUpdateError`.
const NO_UPDATE_ERROR: isize = 1001;

const IDLE: u8 = 0;
/// [업데이트] 를 눌렀다 — 도는 확인이 끝나면 받기를 시작한다.
const ASKED: u8 = 1;
/// 받는 중. 다 받으면 바로 설치하고 다시 켠다.
const RUNNING: u8 = 2;

#[derive(Default)]
struct UpdateState {
    owner: AtomicBool,
    install: AtomicU8,
    /// 사람이 판 번호 줄로 물은 확인 — 닫은 판이어도 다시 알리고, 없으면 「최신」이라고 말한다.
    manual: AtomicBool,
    found: Mutex<Option<(String, bool)>>,
    latest: AtomicBool,
    failure: Mutex<Option<String>>,
}

fn update_state() -> &'static Arc<UpdateState> {
    static STATE: OnceLock<Arc<UpdateState>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(UpdateState::default()))
}

thread_local! {
    // Sparkle 의 위임은 주 스레드에서 오고, 블록은 다음 루프 턴(`tick`)에 부른다 — 위임 안에서
    // 부르면 앱이 Sparkle 의 호출 도중에 종료로 들어간다.
    static READY: RefCell<Option<block2::RcBlock<dyn Fn()>>> = const { RefCell::new(None) };
}

pub(crate) fn owns_installation() -> bool { update_state().owner.load(Ordering::Acquire) }

/// 찾은 새 판과, 그것이 사람이 물은 확인의 답인지.
pub(crate) fn take_found() -> Option<(String, bool)> { update_state().found.lock().ok()?.take() }
/// 사람이 물은 확인에서 새 판이 없었다.
pub(crate) fn take_latest() -> bool { update_state().latest.swap(false, Ordering::AcqRel) }
/// [업데이트] 뒤 받기·설치가 멈춘 까닭.
pub(crate) fn take_failure() -> Option<String> { update_state().failure.lock().ok()?.take() }

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
    #[name = "KasatermPreviewUpdateDelegate"]
    #[ivars = DelegateIvars]
    struct PreviewUpdateDelegate;

    unsafe impl NSObjectProtocol for PreviewUpdateDelegate {}

    impl PreviewUpdateDelegate {
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> Retained<NSString> {
            self.ivars().feed.clone()
        }

        #[unsafe(method(updater:didFindValidUpdate:))]
        fn found(&self, _updater: &AnyObject, item: &AnyObject) {
            let state = &self.ivars().state;
            // 받으러 간 확인도 같은 판을 다시 찾는다 — 그걸 새 알림으로 세우면 받는 동안 같은 판이 또 뜬다.
            if state.install.load(Ordering::Acquire) != IDLE { return; }
            let version: Retained<NSString> = unsafe { msg_send![item, displayVersionString] };
            let manual = state.manual.swap(false, Ordering::AcqRel);
            if let Ok(mut slot) = state.found.lock() { *slot = Some((version.to_string(), manual)); }
        }

        #[unsafe(method(updaterDidNotFindUpdate:))]
        fn not_found(&self, _updater: &AnyObject) {
            let state = &self.ivars().state;
            if state.manual.swap(false, Ordering::AcqRel) { state.latest.store(true, Ordering::Release); }
        }

        #[unsafe(method(updater:willInstallUpdateOnQuit:immediateInstallationBlock:))]
        fn install_on_quit(&self, _updater: &AnyObject, _item: &AnyObject,
            immediate: &block2::DynBlock<dyn Fn()>) -> Bool {
            // 누르지 않은 받기는 없어야 한다. 생기면 Sparkle 의 기본 흐름(창으로 묻기)에 맡긴다.
            if self.ivars().state.install.load(Ordering::Acquire) != RUNNING { return Bool::NO; }
            READY.with(|slot| *slot.borrow_mut() = Some(immediate.copy()));
            Bool::YES
        }

        #[unsafe(method(updater:didAbortWithError:))]
        fn aborted(&self, _updater: &AnyObject, error: &NSError) {
            let state = &self.ivars().state;
            if state.install.compare_exchange(RUNNING, IDLE, Ordering::AcqRel, Ordering::Acquire).is_err() { return; }
            let why = if error.code() == NO_UPDATE_ERROR {
                "새 판을 다시 못 찾았어요".to_string()
            } else {
                error.localizedDescription().to_string()
            };
            if let Ok(mut slot) = state.failure.lock() { *slot = Some(why); }
        }

        #[unsafe(method(updater:didFinishUpdateCycleForUpdateCheck:error:))]
        fn cycle_finished(&self, _updater: &AnyObject, _check: isize, _error: Option<&NSError>) {
            // 받은 판을 붙잡았으면 순환이 멈춰 여기 안 온다. 오면 받기가 설치 없이 끝난 것이다
            // (오류였다면 `didAbortWithError` 가 먼저 걷었다).
            if READY.with(|slot| slot.borrow().is_some()) { return; }
            let state = &self.ivars().state;
            if state.install.compare_exchange(RUNNING, IDLE, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                if let Ok(mut slot) = state.failure.lock() { *slot = Some("받기가 설치 없이 끝났어요".into()); }
            }
        }
    }
);

impl PreviewUpdateDelegate {
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
    _delegate: Option<Retained<PreviewUpdateDelegate>>,
    preview: bool,
    next_probe: Cell<Instant>,
    /// 받기를 위해 자동 확인을 켜 둔 상태인가 — 받기가 멈추면 다시 끈다.
    automatic: Cell<bool>,
}

/// 이 프로세스에만 거는 값이다. 안정판으로 돌아가도 사용자 설정에 남지 않는다.
/// Sparkle 은 자동 확인이 꺼져 있으면 자동 받기도 끈다(`allowsAutomaticUpdates`) — 그래서
/// 받기를 켜는 스위치는 `automatic` 하나다.
unsafe fn configure_session(defaults: &NSUserDefaults, automatic: bool) -> Option<()> {
    let domain = NSString::from_str("NSArgumentDomain");
    let existing: *mut AnyObject = msg_send![defaults, volatileDomainForName: &*domain];
    let copy: *mut AnyObject = msg_send![existing, mutableCopy];
    let values = Retained::from_raw(copy)?;
    let number = AnyClass::get(c"NSNumber")?;
    for (key, on) in [("SUEnableAutomaticChecks", automatic), ("SUAutomaticallyUpdate", true)] {
        let value: *mut AnyObject = msg_send![number, numberWithBool: Bool::new(on)];
        let key = NSString::from_str(key);
        let _: () = msg_send![&*values, setObject: value, forKey: &*key];
    }
    let _: () = msg_send![defaults, setVolatileDomain: &*values, forName: &*domain];
    Some(())
}

/// Sparkle.framework 를 dlopen 해 `SPUStandardUpdaterController` 를 만든다. 반환된 controller 는
/// App 에 **보관해야 한다** — 드롭되면 updater 가 정지한다. framework 가 없으면(dev 빌드) `None`.
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
        let delegate = preview.then(|| PreviewUpdateDelegate::new(update_state().clone()));
        let delegate_ptr = delegate.as_ref().map_or(nil, |value| (&**value as *const PreviewUpdateDelegate).cast_mut().cast());
        if preview { configure_session(&NSUserDefaults::standardUserDefaults(), false)?; }
        // 사용자 드라이버 위임은 걸지 않는다 — 예상 밖으로 Sparkle 이 판을 보여 줘야 할 때는
        // 표준 창이 떠서 사람이 본다(몰래 붙들려 있지 않다).
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithStartingUpdater: Bool::from(!preview),
            updaterDelegate: delegate_ptr,
            userDriverDelegate: nil,
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
        }
        Some(Updater {
            controller, _delegate: delegate, preview,
            next_probe: Cell::new(Instant::now() + FIRST_PROBE),
            automatic: Cell::new(false),
        })
    }
}

/// 주 스레드 루프 턴마다 — 받은 판 설치, 눌린 받기 시작, 멈춘 받기 정리, 시간마다 확인.
pub(crate) fn tick(updater: &Updater) {
    if !updater.preview { return; }
    if let Some(install) = READY.with(|slot| slot.borrow_mut().take()) {
        // 종료·설치·다시 켜기를 Sparkle 이 한다. 종료는 winit `exiting` 을 지나 세션이 저장된다.
        install.call(());
        return;
    }
    let state = update_state();
    unsafe {
        let instance: *mut AnyObject = msg_send![&*updater.controller, updater];
        let busy: Bool = msg_send![instance, sessionInProgress];
        if busy.as_bool() { return; }
        let defaults = NSUserDefaults::standardUserDefaults();
        match state.install.load(Ordering::Acquire) {
            ASKED => {
                if configure_session(&defaults, true).is_some() {
                    updater.automatic.set(true);
                    state.install.store(RUNNING, Ordering::Release);
                    let _: () = msg_send![instance, checkForUpdatesInBackground];
                } else {
                    state.install.store(IDLE, Ordering::Release);
                    if let Ok(mut slot) = state.failure.lock() { *slot = Some("받기를 켜지 못했어요".into()); }
                }
                return;
            }
            IDLE if updater.automatic.get() => {
                // 받다가 멈췄다. 자동 확인을 켜 둔 채면 Sparkle 이 다음 예약 확인에서 사람 없이 받아
                // 종료 때 설치한다 — 끄고 예약도 걷는다.
                let _ = configure_session(&defaults, false);
                updater.automatic.set(false);
                let _: () = msg_send![instance, resetUpdateCycle];
            }
            IDLE => {}
            _ => return,
        }
        let now = Instant::now();
        if now >= updater.next_probe.get() {
            updater.next_probe.set(now + PROBE_EVERY);
            let _: () = msg_send![instance, checkForUpdateInformation];
        }
    }
}

/// 새 판 알림의 [업데이트]. preview 업데이터가 아니면 `false` — 부른 쪽이 다른 길을 고른다.
pub(crate) fn install_now(updater: &Updater) -> bool {
    if !updater.preview { return false; }
    let _ = update_state().install.compare_exchange(IDLE, ASKED, Ordering::AcqRel, Ordering::Acquire);
    true
}

/// "업데이트 확인" 메뉴 → preview 는 확인만 하고 답을 알림으로, 안정판은 표준 다이얼로그.
pub(crate) fn check_for_updates(updater: &Updater) {
    if updater.preview {
        update_state().manual.store(true, Ordering::Release);
        updater.next_probe.set(Instant::now());
        return;
    }
    unsafe {
        let nil: *mut AnyObject = std::ptr::null_mut();
        let _: () = msg_send![&*updater.controller, checkForUpdates: nil];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;

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

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "KasatermTestAppcastItem"]
        struct TestItem;

        impl TestItem {
            #[unsafe(method_id(displayVersionString))]
            fn display_version(&self) -> Retained<NSString> { NSString::from_str("0.2.18") }
        }
    );

    fn ask_install_on_quit(delegate: &PreviewUpdateDelegate, called: &Arc<AtomicBool>) -> bool {
        let dummy = NSObject::new();
        let observed = called.clone();
        let immediate = block2::RcBlock::new(move || { observed.store(true, Ordering::Release); });
        let reply: Bool = unsafe { msg_send![delegate, updater: &*dummy,
            willInstallUpdateOnQuit: &*dummy, immediateInstallationBlock: &*immediate] };
        reply.as_bool()
    }

    #[test]
    fn found_versions_wait_for_the_person_and_only_their_install_runs() {
        let state = Arc::new(UpdateState::default());
        let delegate = PreviewUpdateDelegate::new(state.clone());
        let dummy = NSObject::new();
        let item: Retained<TestItem> = unsafe { msg_send![TestItem::class(), new] };
        let _: () = unsafe { msg_send![&*delegate, updater: &*dummy, didFindValidUpdate: &*item] };
        assert_eq!(state.found.lock().unwrap().take(), Some(("0.2.18".to_string(), false)));

        // 누르기 전 받기는 붙잡지 않는다 — Sparkle 의 표준 흐름이 사람에게 묻는다.
        let called = Arc::new(AtomicBool::new(false));
        assert!(!ask_install_on_quit(&delegate, &called));
        assert!(READY.with(|slot| slot.borrow().is_none()));

        // 받는 중에 같은 판을 다시 찾아도 알림을 또 세우지 않고, 다 받으면 다음 턴에 설치한다.
        state.install.store(RUNNING, Ordering::Release);
        let _: () = unsafe { msg_send![&*delegate, updater: &*dummy, didFindValidUpdate: &*item] };
        assert!(state.found.lock().unwrap().is_none());
        assert!(ask_install_on_quit(&delegate, &called));
        assert!(!called.load(Ordering::Acquire), "위임 안에서 설치를 부르면 Sparkle 호출 도중에 앱이 꺼진다");
        READY.with(|slot| slot.borrow_mut().take().expect("설치 블록").call(()));
        assert!(called.load(Ordering::Acquire));

        let feed: Retained<NSString> = unsafe { msg_send![&*delegate, feedURLStringForUpdater: &*dummy] };
        assert_eq!(feed.to_string(), PREVIEW_FEED);
    }

    #[test]
    fn a_failed_install_is_reported_once_and_probe_answers_reach_the_menu() {
        let state = Arc::new(UpdateState::default());
        let delegate = PreviewUpdateDelegate::new(state.clone());
        let dummy = NSObject::new();
        let error = NSError::new(-1009, &NSString::from_str("NSURLErrorDomain"));
        // 확인만 하다 멈춘 것은 사람이 누른 일이 아니라 알리지 않는다.
        let _: () = unsafe { msg_send![&*delegate, updater: &*dummy, didAbortWithError: &*error] };
        assert!(state.failure.lock().unwrap().is_none());
        state.install.store(RUNNING, Ordering::Release);
        let _: () = unsafe { msg_send![&*delegate, updater: &*dummy, didAbortWithError: &*error] };
        assert!(state.failure.lock().unwrap().take().is_some());
        assert_eq!(state.install.load(Ordering::Acquire), IDLE);

        state.install.store(RUNNING, Ordering::Release);
        let nil: *const NSError = std::ptr::null();
        let _: () = unsafe { msg_send![&*delegate, updater: &*dummy, didFinishUpdateCycleForUpdateCheck: 1isize, error: nil] };
        assert_eq!(state.install.load(Ordering::Acquire), IDLE, "설치 없이 끝난 받기는 자동 확인을 걷게 된다");
        assert!(state.failure.lock().unwrap().take().is_some());

        let _: () = unsafe { msg_send![&*delegate, updaterDidNotFindUpdate: &*dummy] };
        assert!(!state.latest.load(Ordering::Acquire), "시간마다 도는 확인은 「최신」을 말하지 않는다");
        state.manual.store(true, Ordering::Release);
        let _: () = unsafe { msg_send![&*delegate, updaterDidNotFindUpdate: &*dummy] };
        assert!(state.latest.load(Ordering::Acquire));
    }

    #[test]
    fn session_options_never_persist_and_download_only_with_checks_on() {
        let suite = NSString::from_str(&format!("com.kasa.kasaterm.preview-test.{}", uuid::Uuid::new_v4()));
        let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)).unwrap();
        let before = defaults.persistentDomainForName(&suite);
        let checks = NSString::from_str("SUEnableAutomaticChecks");
        let download = NSString::from_str("SUAutomaticallyUpdate");
        unsafe { configure_session(&defaults, false).unwrap(); }
        assert!(!defaults.boolForKey(&checks));
        assert!(defaults.objectForKey(&checks).is_some(), "값이 없으면 Sparkle 이 자동 확인 허락 창을 띄운다");
        unsafe { configure_session(&defaults, true).unwrap(); }
        assert!(defaults.boolForKey(&checks) && defaults.boolForKey(&download));
        assert_eq!(defaults.persistentDomainForName(&suite), before);
        defaults.removeVolatileDomainForName(&NSString::from_str("NSArgumentDomain"));
    }
}
