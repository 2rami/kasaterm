//! macOS Sparkle 자동 업데이트 FFI.
//!
//! Sparkle 은 objc2 바인딩이 없어 raw class lookup(`macos_open.rs` 와 같은 패턴)으로
//! 부른다. `build.rs` 링크 대신 런타임 `dlopen` 을 쓴다 — dev 빌드(`cargo run`)엔
//! Sparkle.framework 가 없어 dlopen 이 실패하고 graceful no-op(업데이터 없이 정상
//! 기동), `.app` 빌드(Contents/Frameworks/Sparkle.framework)에서만 활성화된다.
//!
//! 새 판 안내·받기·진행 막대·설치·다시 켜기는 Sparkle 표준 창이 한다. preview 기기는 켠 지 10초 뒤와
//! 한 시간마다 확인하고, 받기는 사람이 표준 창에서 설치를 눌렀을 때만 한다 — Sparkle 은 한 번 받은 판을
//! 종료 때 반드시 설치하므로, 자동 받기를 끄는 것이 몰래 바뀌지 않게 하는 유일한 막음이다.
//!
//! 0.2.18~0.2.27 은 자체 알림의 [업데이트] 로 이 프로세스에 자동 받기를 켜려 했는데, Sparkle 은 자동 받기
//! 대신 표준 창 경로를 골랐고, 표준 창은 일반 앱이면 「앱이 다시 활성화될 때」까지 미뤄진다. 앱 안에서
//! 누른 사람에겐 진행 표시 없이 아무 일도 안 일어났다(2026-10-02 리그 재현).
//!
//! 표준 창은 그래서 우리가 띄운다: 사람이 이 앱을 보고 있고 타자를 멈춘 틈에. 타자 중에 키 창을 빼앗으면
//! Return 이 「업데이트 설치」를 누른다. 다시 켜기 직전엔 끊길 일을 한 번 더 묻는다(`update_notice.rs`).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSString, NSUserDefaults};

const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-preview.xml";
/// 켠 뒤 첫 확인까지. 창이 서고 세션이 돌아온 뒤에 표준 창이 서게 한다.
const FIRST_PROBE: Duration = Duration::from_secs(10);
const PROBE_EVERY: Duration = Duration::from_secs(3600);
/// 마지막 키 입력 뒤 이만큼 쉬어야 표준 창을 앞으로 낸다.
const TYPING_PAUSE: f64 = 2.0;

#[derive(Default)]
struct UpdateState {
    owner: AtomicBool,
    /// 찾은 판을 표준 창으로 보일 차례다.
    show_pending: AtomicBool,
    /// Sparkle 이 다시 켜려 한다 — 부른 쪽이 끊길 일을 보고 `answer_relaunch` 로 답한다.
    relaunch_asked: AtomicBool,
    /// 「나중에」 — 표준 창을 걷고 알린다.
    later: AtomicBool,
    postponed: AtomicBool,
}

fn update_state() -> &'static UpdateState {
    static STATE: OnceLock<UpdateState> = OnceLock::new();
    STATE.get_or_init(UpdateState::default)
}

thread_local! {
    // Sparkle 의 위임은 주 스레드에서 오고, 블록은 위임 밖에서 부른다 — 위임 안에서 부르면 앱이 Sparkle 의
    // 호출 도중에 종료로 들어간다. 「나중에」 뒤에도 쥐고 있다가 「업데이트 확인」에서 다시 묻는다.
    static RELAUNCH: RefCell<Option<block2::RcBlock<dyn Fn()>>> = const { RefCell::new(None) };
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    fn CFRelease(cf: *const std::ffi::c_void);
}

pub(crate) fn owns_installation() -> bool { update_state().owner.load(Ordering::Acquire) }

/// Sparkle 이 받은 판을 설치하고 다시 켜려 한다 — 한 번만 `true`.
pub(crate) fn take_relaunch_request() -> bool { update_state().relaunch_asked.swap(false, Ordering::AcqRel) }
/// 「나중에」를 골라 다음 종료 때 설치된다 — 한 번만 `true`.
pub(crate) fn take_postponed() -> bool { update_state().postponed.swap(false, Ordering::AcqRel) }

/// 다시 켜도 되는지에 대한 답. `true` 면 Sparkle 이 종료·설치·다시 켜기를 이어 간다(종료는 winit `exiting`
/// 을 지나 세션이 저장된다). `false` 면 표준 창만 걷는다 — 받은 판은 다음에 끌 때 설치된다.
pub(crate) fn answer_relaunch(go: bool) {
    if !go {
        update_state().later.store(true, Ordering::Release);
        return;
    }
    if let Some(install) = RELAUNCH.with(|slot| slot.borrow_mut().take()) {
        install.call(());
    }
}

fn preview_opted_in(settings: &serde_json::Value) -> bool {
    settings["update_channel"].as_str() == Some("preview")
        && settings["automatic_update_on_quit"].as_bool() == Some(true)
}

fn installed_app(exe: &Path, home: &Path, verification: bool) -> bool {
    !verification && (exe == home.join("Applications/kasaterm.app/Contents/MacOS/kasaterm")
        || exe == Path::new("/Applications/kasaterm.app/Contents/MacOS/kasaterm"))
}

/// 검증 리그 전용 피드(`docs/verify-app.md` 「업데이트 리그」). 루프백만 받는다 — 리그 번들은 시험 키를
/// 담아 그 키로 서명한 판만 믿고, 본판은 이 값이 있어도 운영 키로 서명된 판밖에 못 받는다.
fn rig_feed(value: Option<&str>) -> Option<String> {
    let feed = value?;
    let port = feed.strip_prefix("http://127.0.0.1:")?.split('/').next()?;
    port.parse::<u16>().ok()?;
    Some(feed.to_string())
}

/// 리그 전용 — Sparkle 창은 AppKit 이라 앱 캡처(wgpu 프레임)에 안 잡히고, 화면 녹화 권한 없이는 밖에서 찍을
/// 길도 없다. 제 창은 권한 없이 창 서버에서 뜰 수 있어(시트 포함) `KASATERM_UPDATE_RIG_SHOTS` 면 달라질
/// 때마다 PNG 로 남기고, `KASATERM_UPDATE_RIG_PRESS_MS` 면 그만큼 그대로인 창의 기본 단추(Return)를 누른다.
fn rig_watch_windows() {
    use std::hash::{Hash, Hasher};
    type CreateImage = unsafe extern "C" fn(objc2_foundation::NSRect, u32, u32, u32) -> *mut AnyObject;
    struct Seen { digest: u64, since: Instant, pressed: bool }
    thread_local! {
        static SEEN: RefCell<(Option<Instant>, std::collections::HashMap<isize, Seen>, u32)> =
            RefCell::new((None, std::collections::HashMap::new(), 0));
    }
    let shots = std::env::var_os("KASATERM_UPDATE_RIG_SHOTS").map(std::path::PathBuf::from);
    let press = std::env::var("KASATERM_UPDATE_RIG_PRESS_MS").ok().and_then(|v| v.parse().ok()).map(Duration::from_millis);
    if shots.is_none() && press.is_none() { return; }
    SEEN.with(|seen| unsafe {
        let mut seen = seen.borrow_mut();
        let now = Instant::now();
        if seen.0.is_some_and(|at| now < at) { return; }
        seen.0 = Some(now + Duration::from_millis(300));
        // SDK 에선 ScreenCaptureKit 로 밀려났지만 제 창 한 장은 이 함수가 권한 없이 뜬다.
        let symbol = libc::dlsym(libc::RTLD_DEFAULT, c"CGWindowListCreateImage".as_ptr());
        if symbol.is_null() { return; }
        let create: CreateImage = std::mem::transmute(symbol);
        let (Some(app_class), Some(dict_class), Some(rep_class)) = (AnyClass::get(c"NSApplication"),
            AnyClass::get(c"NSDictionary"), AnyClass::get(c"NSBitmapImageRep")) else { return };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        let windows: *mut AnyObject = msg_send![app, windows];
        let props: *mut AnyObject = msg_send![dict_class, dictionary];
        let count: usize = msg_send![windows, count];
        for i in 0..count {
            let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
            let visible: Bool = msg_send![window, isVisible];
            let content: *mut AnyObject = msg_send![window, contentView];
            if !visible.as_bool() || content.is_null() { continue; }
            if (*content).class().name().to_string_lossy().contains("Winit") { continue; }
            let number: isize = msg_send![window, windowNumber];
            let null_rect = objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(f64::INFINITY, f64::INFINITY), objc2_foundation::NSSize::new(0.0, 0.0));
            // kCGWindowListOptionIncludingWindow, kCGWindowImageBoundsIgnoreFraming
            let image = create(null_rect, 8, number as u32, 1);
            if image.is_null() { continue; }
            let rep: *mut AnyObject = msg_send![rep_class, alloc];
            let rep: Option<Retained<AnyObject>> = Retained::from_raw(msg_send![rep, initWithCGImage: image]);
            CFRelease(image.cast());
            let Some(rep) = rep else { continue };
            // NSBitmapImageFileTypePNG
            let data: *mut AnyObject = msg_send![&*rep, representationUsingType: 4usize, properties: props];
            if data.is_null() { continue; }
            let len: usize = msg_send![data, length];
            let bytes: *const u8 = msg_send![data, bytes];
            let png = std::slice::from_raw_parts(bytes, len);
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            png.hash(&mut hasher);
            let digest = hasher.finish();
            if seen.1.get(&number).is_none_or(|old| old.digest != digest) {
                seen.1.insert(number, Seen { digest, since: now, pressed: false });
                seen.2 += 1;
                if let Some(dir) = shots.as_ref() {
                    let _ = std::fs::write(dir.join(format!("{:03}-w{number}.png", seen.2)), png);
                }
                continue;
            }
            let Some(after) = press else { continue };
            let entry = seen.1.get_mut(&number).expect("방금 본 창");
            if entry.pressed || now.duration_since(entry.since) < after { continue; }
            let cell: *mut AnyObject = msg_send![window, defaultButtonCell];
            if cell.is_null() { continue; }
            entry.pressed = true;
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![cell, performClick: nil];
            eprintln!("[update-rig] w{number} 기본 단추 누름");
        }
    });
}

fn writable_install_parent(exe: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Some(parent) = exe.ancestors().nth(4) else { return false };
    let Ok(path) = std::ffi::CString::new(parent.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) == 0 }
}

struct DelegateIvars {
    /// preview 피드. `None` 이면 Info.plist 의 안정판 피드.
    feed: Option<Retained<NSString>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "KasatermUpdateDelegate"]
    #[ivars = DelegateIvars]
    struct UpdateDelegate;

    unsafe impl NSObjectProtocol for UpdateDelegate {}

    impl UpdateDelegate {
        #[unsafe(method_id(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> Option<Retained<NSString>> {
            self.ivars().feed.clone()
        }

        #[unsafe(method(updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:))]
        fn postpone_relaunch(&self, _updater: &AnyObject, _item: &AnyObject,
            install: &block2::DynBlock<dyn Fn()>) -> Bool {
            RELAUNCH.with(|slot| *slot.borrow_mut() = Some(install.copy()));
            update_state().relaunch_asked.store(true, Ordering::Release);
            Bool::YES
        }

        #[unsafe(method(supportsGentleScheduledUpdateReminders))]
        fn gentle_reminders(&self) -> Bool { Bool::YES }

        #[unsafe(method(standardUserDriverShouldHandleShowingScheduledUpdate:andInImmediateFocus:))]
        fn should_show_now(&self, _item: &AnyObject, _immediate_focus: Bool) -> Bool {
            // Sparkle 은 일반 앱이면 다시 활성화될 때까지 미룬다 — 앱 안에 있는 사람에겐 끝내 안 뜬다.
            Bool::NO
        }

        #[unsafe(method(standardUserDriverWillHandleShowingUpdate:forUpdate:state:))]
        fn will_show(&self, handled: Bool, _item: &AnyObject, _state: &AnyObject) {
            if !handled.as_bool() { update_state().show_pending.store(true, Ordering::Release); }
        }

        #[unsafe(method(standardUserDriverWillFinishUpdateSession))]
        fn session_finished(&self) {
            update_state().show_pending.store(false, Ordering::Release);
        }
    }
);

impl UpdateDelegate {
    fn new(feed: Option<&str>) -> Retained<Self> {
        let delegate = Self::alloc().set_ivars(DelegateIvars { feed: feed.map(NSString::from_str) });
        unsafe { msg_send![super(delegate), init] }
    }
}

pub(crate) struct Updater {
    controller: Retained<AnyObject>,
    // Sparkle only holds weak delegate references.
    _delegate: Retained<UpdateDelegate>,
    preview: bool,
    /// 리그는 배경(Accessory) 앱이라 활성이 되지 않는다 — 활성 조건 없이 표준 창을 낸다.
    rig: bool,
    next_probe: Cell<Instant>,
}

/// 이 프로세스에만 거는 값이다. 안정판으로 돌아가도 사용자 설정에 남지 않는다. 확인 예약은 `tick` 이
/// 하고, 자동 확인이 꺼져 있으면 Sparkle 은 자동 받기도 끈다(`allowsAutomaticUpdates`) — 표준 창의
/// 「앞으로 자동으로 받기」 칸도 그래서 숨는다. 예전 판이 기기 설정에 남긴 자동 받기 값도 여기서 덮는다.
unsafe fn configure_session(defaults: &NSUserDefaults) -> Option<()> {
    let domain = NSString::from_str("NSArgumentDomain");
    let existing: *mut AnyObject = msg_send![defaults, volatileDomainForName: &*domain];
    let copy: *mut AnyObject = msg_send![existing, mutableCopy];
    let values = Retained::from_raw(copy)?;
    let number = AnyClass::get(c"NSNumber")?;
    for key in ["SUEnableAutomaticChecks", "SUAutomaticallyUpdate"] {
        let value: *mut AnyObject = msg_send![number, numberWithBool: Bool::NO];
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
    let isolated = crate::verification_run() || crate::version::isolated_environment(|key| std::env::var_os(key).is_some());
    let rig = rig_feed(std::env::var("KASATERM_UPDATE_RIG_FEED").ok().as_deref());
    if rig.is_none() && !installed_app(&exe, &home, isolated) { return None; }
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
        let delegate = UpdateDelegate::new(preview.then(|| rig.as_deref().unwrap_or(PREVIEW_FEED)));
        let delegate_ptr: *mut AnyObject = (&*delegate as *const UpdateDelegate).cast_mut().cast();
        if preview { configure_session(&NSUserDefaults::standardUserDefaults())?; }
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
        }
        Some(Updater {
            controller, _delegate: delegate, preview, rig: rig.is_some(),
            next_probe: Cell::new(Instant::now() + FIRST_PROBE),
        })
    }
}

/// 주 스레드 루프 턴마다 — 미룬 표준 창 띄우기, 「나중에」 정리, preview 의 시간마다 확인.
pub(crate) fn tick(updater: &Updater) {
    rig_watch_windows();
    let state = update_state();
    unsafe {
        let driver: *mut AnyObject = msg_send![&*updater.controller, userDriver];
        if state.later.swap(false, Ordering::AcqRel) {
            let _: () = msg_send![driver, dismissUpdateInstallation];
            state.postponed.store(true, Ordering::Release);
        }
        if state.show_pending.load(Ordering::Acquire) && (updater.rig || app_active())
            // kCGEventSourceStateHIDSystemState, kCGEventKeyDown
            && CGEventSourceSecondsSinceLastEventType(1, 10) >= TYPING_PAUSE
        {
            state.show_pending.store(false, Ordering::Release);
            let _: () = msg_send![driver, showUpdateInFocus];
        }
        if !updater.preview { return; }
        let now = Instant::now();
        if now < updater.next_probe.get() { return; }
        let instance: *mut AnyObject = msg_send![&*updater.controller, updater];
        let busy: Bool = msg_send![instance, sessionInProgress];
        if busy.as_bool() { return; }
        updater.next_probe.set(now + PROBE_EVERY);
        // 자동 받기가 꺼져 있어 찾기만 하고, 찾으면 표준 창 경로(위임의 `will_show`)로 간다.
        let _: () = msg_send![instance, checkForUpdatesInBackground];
    }
}

unsafe fn app_active() -> bool {
    let Some(app_class) = AnyClass::get(c"NSApplication") else { return false };
    let app: *mut AnyObject = msg_send![app_class, sharedApplication];
    let active: Bool = msg_send![app, isActive];
    active.as_bool()
}

/// "업데이트 확인" 메뉴 → 표준 확인 창(없으면 「최신」 안내까지 Sparkle 이 한다). 「나중에」로 미룬 판이
/// 있으면 확인 대신 다시 켤지를 다시 묻는다.
pub(crate) fn check_for_updates(updater: &Updater) {
    if RELAUNCH.with(|slot| slot.borrow().is_some()) {
        update_state().relaunch_asked.store(true, Ordering::Release);
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
    }

    #[test]
    fn rig_feeds_stay_on_loopback() {
        assert_eq!(rig_feed(Some("http://127.0.0.1:54313/appcast.xml")).as_deref(), Some("http://127.0.0.1:54313/appcast.xml"));
        for feed in [None, Some("https://2rami.github.io/kasaterm/appcast-preview.xml"),
            Some("http://127.0.0.1.evil.example/appcast.xml"), Some("http://127.0.0.1:99999/appcast.xml"),
            Some("http://localhost:8000/appcast.xml")] {
            assert!(rig_feed(feed).is_none(), "{feed:?}");
        }
    }

    #[test]
    fn found_versions_wait_for_us_and_relaunch_waits_for_the_answer() {
        let delegate = UpdateDelegate::new(Some(PREVIEW_FEED));
        let dummy = NSObject::new();
        let feed: Option<Retained<NSString>> = unsafe { msg_send![&*delegate, feedURLStringForUpdater: &*dummy] };
        assert_eq!(feed.map(|f| f.to_string()).as_deref(), Some(PREVIEW_FEED));
        let stable = UpdateDelegate::new(None);
        let feed: Option<Retained<NSString>> = unsafe { msg_send![&*stable, feedURLStringForUpdater: &*dummy] };
        assert!(feed.is_none(), "안정판은 Info.plist 피드를 쓴다");

        // 표준 창을 띄울 때는 우리가 고른다 — Sparkle 의 「다시 활성화될 때」를 쓰지 않는다.
        let gentle: Bool = unsafe { msg_send![&*delegate, supportsGentleScheduledUpdateReminders] };
        assert!(gentle.as_bool());
        let sparkle_shows: Bool = unsafe { msg_send![&*delegate,
            standardUserDriverShouldHandleShowingScheduledUpdate: &*dummy, andInImmediateFocus: Bool::NO] };
        assert!(!sparkle_shows.as_bool());
        let state = update_state();
        let _: () = unsafe { msg_send![&*delegate, standardUserDriverWillHandleShowingUpdate: Bool::NO,
            forUpdate: &*dummy, state: &*dummy] };
        assert!(state.show_pending.load(Ordering::Acquire));
        let _: () = unsafe { msg_send![&*delegate, standardUserDriverWillFinishUpdateSession] };
        assert!(!state.show_pending.load(Ordering::Acquire));

        let called = std::sync::Arc::new(AtomicBool::new(false));
        let observed = called.clone();
        let install = block2::RcBlock::new(move || { observed.store(true, Ordering::Release); });
        let postponed: Bool = unsafe { msg_send![&*delegate, updater: &*dummy,
            shouldPostponeRelaunchForUpdate: &*dummy, untilInvokingBlock: &*install] };
        assert!(postponed.as_bool(), "끊길 일을 묻기 전에는 다시 켜지 않는다");
        assert!(!called.load(Ordering::Acquire), "위임 안에서 설치를 부르면 Sparkle 호출 도중에 앱이 꺼진다");
        assert!(take_relaunch_request());
        assert!(!take_relaunch_request(), "한 번만 묻는다");

        answer_relaunch(false);
        assert!(state.later.swap(false, Ordering::AcqRel));
        assert!(!called.load(Ordering::Acquire), "「나중에」는 다시 켜지 않는다");
        assert!(RELAUNCH.with(|slot| slot.borrow().is_some()), "업데이트 확인에서 다시 물을 수 있게 쥐고 있다");
        answer_relaunch(true);
        assert!(called.load(Ordering::Acquire));
        assert!(RELAUNCH.with(|slot| slot.borrow().is_none()));
    }

    #[test]
    fn session_options_never_persist_and_never_download_on_their_own() {
        let suite = NSString::from_str(&format!("com.kasa.kasaterm.preview-test.{}", uuid::Uuid::new_v4()));
        let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)).unwrap();
        let before = defaults.persistentDomainForName(&suite);
        let checks = NSString::from_str("SUEnableAutomaticChecks");
        let download = NSString::from_str("SUAutomaticallyUpdate");
        unsafe { configure_session(&defaults).unwrap(); }
        assert!(!defaults.boolForKey(&checks) && !defaults.boolForKey(&download));
        assert!(defaults.objectForKey(&checks).is_some(), "값이 없으면 Sparkle 이 자동 확인 허락 창을 띄운다");
        assert_eq!(defaults.persistentDomainForName(&suite), before);
        // 예전 판의 표준 창에서 켠 「자동으로 받기」가 기기 설정에 남아 있어도 이 프로세스에선 꺼져 있다.
        defaults.setBool_forKey(true, &download);
        assert!(!defaults.boolForKey(&download));
        defaults.removePersistentDomainForName(&suite);
        defaults.removeVolatileDomainForName(&NSString::from_str("NSArgumentDomain"));
    }
}
