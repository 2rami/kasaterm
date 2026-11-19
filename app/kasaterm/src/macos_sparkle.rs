//! macOS Sparkle 자동 업데이트 FFI.
//!
//! Sparkle 은 objc2 바인딩이 없어 raw class lookup(`macos_open.rs` 와 같은 패턴)으로
//! 부른다. `build.rs` 링크 대신 런타임 `dlopen` 을 쓴다 — dev 빌드(`cargo run`)엔
//! Sparkle.framework 가 없어 dlopen 이 실패하고 graceful no-op(업데이터 없이 정상
//! 기동), `.app` 빌드(Contents/Frameworks/Sparkle.framework)에서만 활성화된다.
//!
//! 화면은 앱이 그린다 — 표준 창(SPUStandardUserDriver) 대신 사용자 드라이버(`KasatermUserDriver`)를 넘겨, Sparkle 이
//! 찾고·받고·풀고·설치하는 단계를 앱 안 새 판 카드(`update_card.rs`)로 보이고 카드의 단추로 답한다. 피드 확인·EdDSA
//! 서명 확인·설치·다시 켜기는 그대로 Sparkle 몫이다.
//!
//! 받기는 사람이 카드에서 「받기」를 눌렀을 때만 한다 — Sparkle 은 한 번 받은 판을 종료 때 반드시 설치하므로, 자동
//! 받기를 끄는 것이 몰래 바뀌지 않게 하는 유일한 막음이다. 표준 창일 때는 일반 앱이면 「앱이 다시 활성화될 때」까지
//! 창이 미뤄져 앱 안에 있는 사람에겐 끝내 안 뜨기도 했다(2026-10-02 리그 재현) — 카드는 늘 그 자리에 선다.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, ClassType, DefinedClass};
use objc2_foundation::{NSBundle, NSError, NSString, NSUserDefaults};

use crate::update_card::{self, Action, Stage};

const PREVIEW_FEED: &str = "https://2rami.github.io/kasaterm/appcast-preview.xml";
/// 켠 뒤 첫 확인까지. 창이 서고 세션이 돌아온 뒤에 카드가 서게 한다.
const FIRST_PROBE: Duration = Duration::from_secs(10);
const PROBE_EVERY: Duration = Duration::from_secs(3600);

/// `SPUUserUpdateChoice`.
const CHOICE_SKIP: isize = 0;
const CHOICE_INSTALL: isize = 1;
const CHOICE_DISMISS: isize = 2;
/// `SPUUserUpdateStage` — 이미 받은 판·설치 중인 판은 받기를 건너뛴다.
const STAGE_DOWNLOADED: isize = 1;
const STAGE_INSTALLING: isize = 2;

static OWNER: AtomicBool = AtomicBool::new(false);

thread_local! {
    // Sparkle 의 드라이버 호출은 주 스레드에서 오고, 답(블록)은 사람이 카드를 누를 때 주 스레드에서 부른다.
    static CHOICE: RefCell<Option<block2::RcBlock<dyn Fn(isize)>>> = const { RefCell::new(None) };
    static CANCEL: RefCell<Option<block2::RcBlock<dyn Fn()>>> = const { RefCell::new(None) };
    /// 받는 판 — 진행 단계마다 같은 번호를 쓴다.
    static VERSION: RefCell<String> = const { RefCell::new(String::new()) };
    static GOT: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

unsafe extern "C" {
    fn objc_getProtocol(name: *const std::ffi::c_char) -> *const std::ffi::c_void;
    fn class_addProtocol(cls: *const AnyClass, protocol: *const std::ffi::c_void) -> Bool;
}

pub(crate) fn owns_installation() -> bool { OWNER.load(Ordering::Acquire) }

/// 카드의 단추 — 쥐고 있던 Sparkle 의 답을 부른다. 답할 것이 없으면(이미 답했거나 알림뿐인 단계) 카드만 걷는다.
pub(crate) fn respond(action: Action) {
    let choice = |value: isize| CHOICE.with(|slot| slot.borrow_mut().take()).map(|reply| reply.call((value,))).is_some();
    let answered = match action {
        Action::Download | Action::Relaunch => choice(CHOICE_INSTALL),
        Action::Skip => choice(CHOICE_SKIP),
        Action::Later => choice(CHOICE_DISMISS),
        Action::Cancel => CANCEL.with(|slot| slot.borrow_mut().take()).map(|cancel| cancel.call(())).is_some(),
        Action::Close | Action::Releases => false,
    };
    if !answered || matches!(action, Action::Skip | Action::Later | Action::Cancel) {
        update_card::post(None);
    }
}

fn version() -> String { VERSION.with(|v| v.borrow().clone()) }

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
pub(crate) fn rig_watch_windows() {
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
            let cell: *mut AnyObject = if crate::verification_run() {
                std::env::var("KASATERM_UPDATE_RIG_BUTTON").ok()
                    .map(|title| rig_button(content, &title)).unwrap_or_else(|| msg_send![window, defaultButtonCell])
            } else { msg_send![window, defaultButtonCell] };
            if cell.is_null() { continue; }
            entry.pressed = true;
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: () = msg_send![cell, performClick: nil];
            eprintln!("[update-rig] w{number} 기본 단추 누름");
        }
    });
}

unsafe fn rig_button(view: *mut AnyObject, title: &str) -> *mut AnyObject {
    if let Some(button) = AnyClass::get(c"NSButton") {
        let matches: Bool = unsafe { msg_send![view, isKindOfClass: button] };
        if matches.as_bool() {
            let label: *mut NSString = unsafe { msg_send![view, title] };
            if !label.is_null() && unsafe { &*label }.to_string() == title { return view; }
        }
    }
    let children: *mut AnyObject = unsafe { msg_send![view, subviews] };
    let count: usize = unsafe { msg_send![children, count] };
    for i in 0..count {
        let child: *mut AnyObject = unsafe { msg_send![children, objectAtIndex: i] };
        let found = unsafe { rig_button(child, title) };
        if !found.is_null() { return found; }
    }
    std::ptr::null_mut()
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
    }
);

impl UpdateDelegate {
    fn new(feed: Option<&str>) -> Retained<Self> {
        let delegate = Self::alloc().set_ivars(DelegateIvars { feed: feed.map(NSString::from_str) });
        unsafe { msg_send![super(delegate), init] }
    }
}

/// 받은 판의 표시 번호(`SUAppcastItem.displayVersionString`). 모르는 물건이면 빈 값.
unsafe fn item_version(item: &AnyObject) -> String {
    let responds: Bool = msg_send![item, respondsToSelector: objc2::sel!(displayVersionString)];
    if !responds.as_bool() { return String::new(); }
    let v: *mut NSString = msg_send![item, displayVersionString];
    if v.is_null() { String::new() } else { (*v).to_string() }
}

unsafe fn update_stage(state: &AnyObject) -> isize {
    let responds: Bool = msg_send![state, respondsToSelector: objc2::sel!(stage)];
    if responds.as_bool() { msg_send![state, stage] } else { 0 }
}

fn error_text(error: &NSError) -> String {
    let text = error.localizedDescription().to_string();
    if text.is_empty() { "업데이터가 까닭을 말하지 않았어요.".into() } else { text }
}

define_class!(
    /// `SPUUserDriver` — Sparkle 이 단계마다 부르는 화면. 런타임에 그 프로토콜을 단다(`init`).
    #[unsafe(super(NSObject))]
    #[name = "KasatermUserDriver"]
    struct UserDriver;

    unsafe impl NSObjectProtocol for UserDriver {}

    impl UserDriver {
        #[unsafe(method(showUpdatePermissionRequest:reply:))]
        fn permission(&self, _request: &AnyObject, reply: &block2::DynBlock<dyn Fn(*mut AnyObject)>) {
            // 안정판은 Info.plist 가 자동 확인을 이미 정했고, preview 는 이 프로세스에서만 끈다(`configure_session`).
            // 묻는 창을 띄우지 않고 확인만 켠 채 답한다 — 받기는 늘 사람이 카드에서 고른다.
            unsafe {
                let Some(cls) = AnyClass::get(c"SUUpdatePermissionResponse") else { return };
                let alloc: *mut AnyObject = msg_send![cls, alloc];
                let response: *mut AnyObject = msg_send![alloc, initWithAutomaticUpdateChecks: Bool::YES, sendSystemProfile: Bool::NO];
                reply.call((response,));
                let _ = Retained::from_raw(response);
            }
        }

        #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
        fn checking(&self, cancellation: &block2::DynBlock<dyn Fn()>) {
            CANCEL.with(|slot| *slot.borrow_mut() = Some(cancellation.copy()));
            update_card::post(Some(Stage::Checking));
        }

        #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
        fn found(&self, item: &AnyObject, state: &AnyObject, reply: &block2::DynBlock<dyn Fn(isize)>) {
            let (version, stage) = unsafe { (item_version(item), update_stage(state)) };
            VERSION.with(|v| *v.borrow_mut() = version.clone());
            CANCEL.with(|slot| slot.borrow_mut().take());
            CHOICE.with(|slot| *slot.borrow_mut() = Some(reply.copy()));
            update_card::post(Some(match stage {
                // 「나중에」로 미뤄 둔 받은 판 — 다시 받지 않고 다시 켤지만 묻는다.
                STAGE_DOWNLOADED => Stage::Ready { version },
                STAGE_INSTALLING => Stage::Installing { version },
                _ => Stage::Found { version, note: None },
            }));
        }

        #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
        fn notes(&self, _data: &AnyObject) {}

        #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
        fn notes_failed(&self, _error: &NSError) {}

        #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
        fn not_found(&self, _error: &NSError, ack: &block2::DynBlock<dyn Fn()>) {
            CANCEL.with(|slot| slot.borrow_mut().take());
            update_card::post(Some(Stage::Latest));
            ack.call(());
        }

        #[unsafe(method(showUpdaterError:acknowledgement:))]
        fn failed(&self, error: &NSError, ack: &block2::DynBlock<dyn Fn()>) {
            CANCEL.with(|slot| slot.borrow_mut().take());
            CHOICE.with(|slot| slot.borrow_mut().take());
            update_card::post(Some(Stage::Failed { message: error_text(error) }));
            ack.call(());
        }

        #[unsafe(method(showDownloadInitiatedWithCancellation:))]
        fn download_started(&self, cancellation: &block2::DynBlock<dyn Fn()>) {
            CANCEL.with(|slot| *slot.borrow_mut() = Some(cancellation.copy()));
            GOT.with(|g| g.set((0, 0)));
            update_card::post(Some(Stage::Downloading { version: version(), got: 0, total: 0 }));
        }

        #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
        fn download_length(&self, total: u64) {
            GOT.with(|g| g.set((g.get().0, total)));
            update_card::post(Some(Stage::Downloading { version: version(), got: 0, total }));
        }

        #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
        fn download_data(&self, length: u64) {
            let (got, total) = GOT.with(|g| { let (got, total) = g.get(); g.set((got + length, total)); (got + length, total) });
            update_card::post(Some(Stage::Downloading { version: version(), got, total }));
        }

        #[unsafe(method(showDownloadDidStartExtractingUpdate))]
        fn extracting(&self) {
            CANCEL.with(|slot| slot.borrow_mut().take());
            update_card::post(Some(Stage::Preparing { version: version(), progress: 0.0 }));
        }

        #[unsafe(method(showExtractionReceivedProgress:))]
        fn extraction(&self, progress: f64) {
            update_card::post(Some(Stage::Preparing { version: version(), progress: progress.clamp(0.0, 1.0) }));
        }

        #[unsafe(method(showReadyToInstallAndRelaunch:))]
        fn ready(&self, reply: &block2::DynBlock<dyn Fn(isize)>) {
            CHOICE.with(|slot| *slot.borrow_mut() = Some(reply.copy()));
            update_card::post(Some(Stage::Ready { version: version() }));
        }

        #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
        fn installing(&self, _terminated: Bool, _retry: &block2::DynBlock<dyn Fn()>) {
            update_card::post(Some(Stage::Installing { version: version() }));
        }

        #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
        fn installed(&self, _relaunched: Bool, ack: &block2::DynBlock<dyn Fn()>) {
            update_card::post(Some(Stage::Installed { version: crate::version::CURRENT.to_string() }));
            ack.call(());
        }

        #[unsafe(method(showUpdateInFocus))]
        fn in_focus(&self) {}

        #[unsafe(method(dismissUpdateInstallation))]
        fn dismiss(&self) {
            CHOICE.with(|slot| slot.borrow_mut().take());
            CANCEL.with(|slot| slot.borrow_mut().take());
            // 알리기만 하는 카드(최신·실패·바뀜)는 제 시간에 걷힌다 — 세션이 끝났다고 바로 지우지 않는다.
            if !matches!(update_card::current(), Some(Stage::Latest | Stage::Failed { .. } | Stage::Installed { .. })) {
                update_card::post(None);
            }
        }
    }
);

impl UserDriver {
    fn new() -> Retained<Self> {
        let driver = Self::alloc().set_ivars(());
        unsafe { msg_send![super(driver), init] }
    }
}

pub(crate) struct Updater {
    updater: Retained<AnyObject>,
    // Sparkle only holds weak delegate references.
    _driver: Retained<UserDriver>,
    _delegate: Retained<UpdateDelegate>,
    preview: bool,
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

/// Sparkle.framework 를 dlopen 해 앱 드라이버를 단 `SPUUpdater` 를 만든다. 반환된 값은 App 에 **보관해야 한다** —
/// 드롭되면 updater 가 정지한다. framework 가 없으면(dev 빌드) `None`.
pub(crate) fn init() -> Option<Updater> {
    let exe = std::env::current_exe().ok()?;
    let home = kasa_socket::home_dir()?;
    let isolated = crate::verification_run() || crate::version::isolated_environment(|key| std::env::var_os(key).is_some());
    let rig = rig_feed(std::env::var("KASATERM_UPDATE_RIG_FEED").ok().as_deref());
    if rig.is_none() && !installed_app(&exe, &home, isolated) { return None; }
    let preview = crate::version::preview_opted_in(&crate::socket::read_settings());
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
        let cls = AnyClass::get(c"SPUUpdater")?;
        let driver = UserDriver::new();
        let protocol = objc_getProtocol(c"SPUUserDriver".as_ptr());
        if !protocol.is_null() {
            let _ = class_addProtocol(UserDriver::class(), protocol);
        }
        let delegate = UpdateDelegate::new(preview.then(|| rig.as_deref().unwrap_or(PREVIEW_FEED)));
        if preview { configure_session(&NSUserDefaults::standardUserDefaults())?; }
        let bundle = NSBundle::mainBundle();
        let alloc: *mut AnyObject = msg_send![cls, alloc];
        if alloc.is_null() { return None; }
        let obj: *mut AnyObject = msg_send![
            alloc,
            initWithHostBundle: &*bundle,
            applicationBundle: &*bundle,
            userDriver: &*driver,
            delegate: &*delegate,
        ];
        let updater = Retained::from_raw(obj)?;
        let mut error: *mut NSError = std::ptr::null_mut();
        let started: Bool = msg_send![&*updater, startUpdater: &mut error];
        if !started.as_bool() { return None; }
        if preview {
            // Selecting a single writer before the first update cycle also
            // covers a local dist build that finishes after Sparkle stages.
            OWNER.store(true, Ordering::Release);
        }
        Some(Updater { updater, _driver: driver, _delegate: delegate, preview, next_probe: Cell::new(Instant::now() + FIRST_PROBE) })
    }
}

impl Updater {
    /// 미리보기 참여 기기 — 앱이 새 판을 묻지 않고 받고 쉴 때 다시 켠다(`update_notice`).
    pub(crate) fn automatic(&self) -> bool {
        self.preview
    }
}

/// 주 스레드 루프 턴마다 — preview 의 시간마다 확인.
pub(crate) fn tick(updater: &Updater) {
    rig_watch_windows();
    if !updater.preview { return; }
    let now = Instant::now();
    if now < updater.next_probe.get() { return; }
    unsafe {
        let busy: Bool = msg_send![&*updater.updater, sessionInProgress];
        if busy.as_bool() { return; }
        updater.next_probe.set(now + PROBE_EVERY);
        // 자동 받기가 꺼져 있어 찾기만 하고, 찾으면 드라이버의 `found` 로 카드가 선다.
        let _: () = msg_send![&*updater.updater, checkForUpdatesInBackground];
    }
}

/// "업데이트 확인" — 찾는 중 카드부터 선다. 「나중에」로 미룬 받은 판이 있으면 Sparkle 이 다시 켤지를 다시 묻는다.
pub(crate) fn check_for_updates(updater: &Updater) {
    unsafe {
        let can: Bool = msg_send![&*updater.updater, canCheckForUpdates];
        if can.as_bool() {
            let _: () = msg_send![&*updater.updater, checkForUpdates];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_feed_is_ours_only_on_preview() {
        let dummy = NSObject::new();
        let delegate = UpdateDelegate::new(Some(PREVIEW_FEED));
        let feed: Option<Retained<NSString>> = unsafe { msg_send![&*delegate, feedURLStringForUpdater: &*dummy] };
        assert_eq!(feed.map(|f| f.to_string()).as_deref(), Some(PREVIEW_FEED));
        let stable = UpdateDelegate::new(None);
        let feed: Option<Retained<NSString>> = unsafe { msg_send![&*stable, feedURLStringForUpdater: &*dummy] };
        assert!(feed.is_none(), "안정판은 Info.plist 피드를 쓴다");
    }

    /// Sparkle 이 부르는 차례 그대로 — 찾음 → 받기 → 진행 → 준비 → 다시 켜기. 답은 카드의 단추가 부를 때만 간다.
    #[test]
    fn the_card_answers_sparkle_only_when_a_person_presses() {
        let driver = UserDriver::new();
        let dummy = NSObject::new();
        let answered = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = answered.clone();
        let reply = block2::RcBlock::new(move |choice: isize| seen.lock().unwrap().push(choice));
        let _: () = unsafe { msg_send![&*driver, showUpdateFoundWithAppcastItem: &*dummy, state: &*dummy, reply: &*reply] };
        assert!(matches!(update_card::current(), Some(Stage::Found { .. })));
        assert!(answered.lock().unwrap().is_empty(), "찾기만 했다 — 받기는 사람이 누를 때");
        respond(Action::Download);
        assert_eq!(*answered.lock().unwrap(), vec![CHOICE_INSTALL]);

        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let cancel = block2::RcBlock::new(move || flag.store(true, Ordering::Release));
        let _: () = unsafe { msg_send![&*driver, showDownloadInitiatedWithCancellation: &*cancel] };
        let _: () = unsafe { msg_send![&*driver, showDownloadDidReceiveExpectedContentLength: 4_000u64] };
        let _: () = unsafe { msg_send![&*driver, showDownloadDidReceiveDataOfLength: 1_000u64] };
        assert!(matches!(update_card::current(), Some(Stage::Downloading { got: 1_000, total: 4_000, .. })));
        let _: () = unsafe { msg_send![&*driver, showDownloadDidStartExtractingUpdate] };
        assert!(matches!(update_card::current(), Some(Stage::Preparing { .. })));
        respond(Action::Cancel);
        assert!(!cancelled.load(Ordering::Acquire), "푸는 중엔 받기 취소가 이미 지난 일이다");

        let _: () = unsafe { msg_send![&*driver, showReadyToInstallAndRelaunch: &*reply] };
        assert!(matches!(update_card::current(), Some(Stage::Ready { .. })));
        respond(Action::Later);
        assert_eq!(*answered.lock().unwrap(), vec![CHOICE_INSTALL, CHOICE_DISMISS], "나중에 = 끌 때 설치");
        assert_eq!(update_card::current(), None);
        respond(Action::Relaunch);
        assert_eq!(answered.lock().unwrap().len(), 2, "한 번 답한 블록은 다시 안 부른다");

        let _: () = unsafe { msg_send![&*driver, dismissUpdateInstallation] };
        update_card::post(None);
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
