//! 덮어도 마저 하기 — 학생이 일하는 동안 잠자기를 막아 두고(`Armed`), 덮이면 깨어 있는 채 하던 일이 끝나기를
//! 기다렸다가(`Away`) 일이 1분 넘게 멎거나 상한이 차면 재운다. 다시 열면 막음만 이어 간다.
//! 설치물·pmset 명령은 `kasa_socket::lid_away` 다.
//!
//! 덮였다고 학생을 다른 기기로 옮기지 않는다 — 다시 열었을 때 칸이 다른 기기 거울로 바뀌어 있고 미커밋
//! 변경은 맥북에 남아, 그 자리에서 마저 끝내고 자는 편을 골랐다(2026-10-22). 사람이 직접 신청한 이사는
//! 하던 일로 세어 끝날 때까지 깨어 있는다. 외부 모니터를 꽂고 덮은 채 쓰는 자리는 떠난 것이 아니다.
use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
/// 일이 이만큼 멎어야 끝난 것으로 본다 — 턴 사이 잠깐 쉬거나 다른 학생의 쪽지를 기다리는 사이에 막음을
/// 풀었다 걸거나, 덮인 채 하던 일 가운데서 재우지 않게.
const QUIET_GRACE: Duration = Duration::from_secs(60);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
const BATTERY_FLOOR: u8 = 20;
pub(crate) const DEFAULT_MINUTES: u64 = 30;
pub(crate) const MINUTE_CHOICES: [u64; 3] = [10, 30, 60];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Armed { quiet_since: Option<Instant> },
    Away { since: Instant, quiet_since: Option<Instant> },
}

#[derive(Clone, Copy, Debug)]
struct Sense {
    /// 켜져 있고, sudo 가 허락하고, 전원·열이 괜찮다.
    allowed: bool,
    /// 이 기기 칸에서 학생이 일하거나, 사람이 신청한 이사가 예약·실행 중이다.
    busy: bool,
    /// 덮였고 외부 모니터도 없다.
    departed: bool,
    cap: Duration,
    now: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Arm,
    Disarm,
    Sleep,
}

fn step(phase: Phase, s: Sense) -> (Phase, Option<Act>) {
    let quiet = |since: Option<Instant>| if s.busy { None } else { Some(since.unwrap_or(s.now)) };
    let rested = |since: Option<Instant>| since.is_some_and(|t| s.now.duration_since(t) >= QUIET_GRACE);
    match phase {
        Phase::Idle => {
            // 덮인 채 잠깐 깬 것(Power Nap)에 다시 걸면 가방 속에서 상한만큼 깨어 있게 된다.
            if s.allowed && s.busy && !s.departed {
                (Phase::Armed { quiet_since: None }, Some(Act::Arm))
            } else {
                (Phase::Idle, None)
            }
        }
        Phase::Armed { quiet_since } => {
            if s.departed {
                return if s.allowed {
                    (Phase::Away { since: s.now, quiet_since: quiet(quiet_since) }, None)
                } else {
                    (Phase::Idle, Some(Act::Sleep))
                };
            }
            if !s.allowed {
                return (Phase::Idle, Some(Act::Disarm));
            }
            let quiet_since = quiet(quiet_since);
            if rested(quiet_since) {
                (Phase::Idle, Some(Act::Disarm))
            } else {
                (Phase::Armed { quiet_since }, None)
            }
        }
        Phase::Away { since, quiet_since } => {
            let quiet_since = quiet(quiet_since);
            if !s.departed {
                return (Phase::Armed { quiet_since }, None);
            }
            if !s.allowed || s.now.duration_since(since) >= s.cap || rested(quiet_since) {
                (Phase::Idle, Some(Act::Sleep))
            } else {
                (Phase::Away { since, quiet_since }, None)
            }
        }
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "Idle",
        Phase::Armed { .. } => "Armed",
        Phase::Away { .. } => "Away",
    }
}

#[derive(Clone)]
struct Config {
    on: bool,
    cap: Duration,
}

struct State {
    phase: Phase,
    booted: bool,
    last_tick: Option<Instant>,
    beat_at: Option<Instant>,
    arm_retry_at: Option<Instant>,
    config: Option<(Instant, Config)>,
    safe: Option<(Instant, bool)>,
    activity: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
}

thread_local! {
    static STATE: RefCell<State> = {
        RefCell::new(State {
            phase: Phase::Idle,
            booted: false,
            last_tick: None,
            beat_at: None,
            arm_retry_at: None,
            config: None,
            safe: None,
            activity: None,
        })
    };
}

fn read_config() -> Config {
    config_from(&socket::read_settings())
}

fn config_from(settings: &serde_json::Value) -> Config {
    // 덮으면 이사이던 때는 옮길 기기를 고른 것이 켬이었다 — 그 사람은 켠 채로 이어 간다.
    let on = settings["lid_away"]
        .as_bool()
        .unwrap_or_else(|| settings["lid_away_machine"].as_str().is_some_and(|m| !m.trim().is_empty()));
    let minutes = settings["lid_away_minutes"].as_u64().filter(|m| *m > 0).unwrap_or(DEFAULT_MINUTES);
    Config { on, cap: Duration::from_secs(minutes * 60) }
}

fn config(st: &mut State, now: Instant) -> Config {
    match &st.config {
        Some((at, c)) if now.duration_since(*at) < Duration::from_secs(5) => c.clone(),
        _ => {
            let c = read_config();
            st.config = Some((now, c.clone()));
            c
        }
    }
}

/// 설정 화면이 값을 바꾼 직후 — 5초 캐시를 기다리지 않게.
pub(crate) fn forget_config() {
    STATE.with(|s| s.borrow_mut().config = None);
}

pub(crate) fn enabled() -> bool {
    read_config().on
}

pub(crate) fn cap_minutes() -> u64 {
    read_config().cap.as_secs() / 60
}

/// 0 모름 · 1 없음 · 2 있음. sudo 를 띄워야 알 수 있어 뒤에서 잰다.
static READY: AtomicU8 = AtomicU8::new(0);
static READY_CHECKING: AtomicBool = AtomicBool::new(false);
static READY_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// sudoers 규칙이 깔렸나. 모르면 `None` 이고 뒤에서 재기 시작한다 — 그리기 경로에서 불러도 된다.
pub(crate) fn sudo_ready() -> Option<bool> {
    let stale = READY_AT
        .lock()
        .map(|at| at.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)))
        .unwrap_or(false);
    if stale && !READY_CHECKING.swap(true, Ordering::AcqRel) {
        std::thread::spawn(|| {
            let ok = kasa_socket::lid_away::sudo_ready();
            READY.store(if ok { 2 } else { 1 }, Ordering::Release);
            if let Ok(mut at) = READY_AT.lock() {
                *at = Some(Instant::now());
            }
            READY_CHECKING.store(false, Ordering::Release);
        });
    }
    match READY.load(Ordering::Acquire) {
        2 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// 설정 화면의 한 줄 — 지금 무엇을 하고 있나.
pub(crate) fn status_line() -> Option<&'static str> {
    STATE.with(|s| match s.borrow().phase {
        Phase::Idle => None,
        Phase::Armed { .. } => Some("학생이 일하는 중이라 잠자기를 막아 두었어요"),
        Phase::Away { .. } => Some("덮인 채 학생이 하던 일을 마치는 중이에요"),
    })
}

/// 깨어 있는 동안은 1초마다 루프를 깨운다 — 덮이면 화면 이벤트가 없어 `Wait` 에 잠든다.
pub(crate) fn wake_deadline() -> Option<Instant> {
    STATE.with(|s| (s.borrow().phase != Phase::Idle).then(|| Instant::now() + TICK))
}

fn write_heartbeat() {
    if let Some(hb) = kasa_socket::lid_away::heartbeat_path() {
        if let Some(dir) = hb.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(hb, format!("{}\n", std::process::id()));
    }
}

fn clear_heartbeat() {
    if let Some(hb) = kasa_socket::lid_away::heartbeat_path() {
        let _ = std::fs::remove_file(hb);
    }
}

/// 막음을 풀고 박동을 지운다. 못 풀면 박동을 남겨 감시가 다시 시도하게 한다.
fn release() -> anyhow::Result<()> {
    kasa_socket::lid_away::set_sleep_disabled(false)?;
    clear_heartbeat();
    Ok(())
}

/// 앱이 끝날 때. 막아 둔 채 나가면 감시가 90초 뒤에야 푼다.
pub(crate) fn shutdown() {
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        if st.phase != Phase::Idle {
            if let Err(e) = release() {
                eprintln!("[lid-away] 끝내며 잠자기 막음을 못 풀었다: {e:#}");
            }
            st.phase = Phase::Idle;
            hold_app_nap_off(&mut st, false);
        }
    });
}

impl App {
    pub(crate) fn tick_lid_away(&mut self) {
        let now = Instant::now();
        let due = STATE.with(|s| {
            let mut st = s.borrow_mut();
            if st.last_tick.is_some_and(|t| now.duration_since(t) < TICK) {
                return false;
            }
            st.last_tick = Some(now);
            if !st.booted {
                st.booted = true;
                // 지난 실행이 막아 둔 채 죽었다.
                if kasa_socket::lid_away::heartbeat_path().is_some_and(|hb| hb.exists()) {
                    if let Err(e) = release() {
                        eprintln!("[lid-away] 지난 실행의 잠자기 막음을 못 풀었다: {e:#}");
                    }
                }
            }
            true
        });
        if !due {
            return;
        }
        let (phase, cfg) = STATE.with(|s| {
            let mut st = s.borrow_mut();
            (st.phase, config(&mut st, now))
        });
        if !cfg.on && phase == Phase::Idle {
            return;
        }
        let working = self.lid_away_working_panes();
        let migrating = self.migrate_running_any() || !self.migrate_queue.is_empty()
            || self.migrate_handoff.is_some();
        let departed = probe::lid_closed() && !probe::external_display();
        let safe = STATE.with(|s| {
            let mut st = s.borrow_mut();
            match st.safe {
                Some((at, ok)) if now.duration_since(at) < Duration::from_secs(10) => ok,
                _ => {
                    let ok = probe::power_ok(BATTERY_FLOOR) && !probe::thermal_serious();
                    st.safe = Some((now, ok));
                    ok
                }
            }
        });
        let arm_blocked = STATE.with(|s| s.borrow().arm_retry_at.is_some_and(|t| now < t));
        let sense = Sense {
            allowed: cfg.on && sudo_ready() == Some(true) && safe && !arm_blocked,
            busy: !working.is_empty() || migrating,
            departed,
            cap: cfg.cap,
            now,
        };
        let (next, act) = step(phase, sense);
        STATE.with(|s| s.borrow_mut().phase = next);
        if act.is_some() || phase_name(phase) != phase_name(next) {
            eprintln!("[lid-away] {act:?} {} → {} (일하는 칸 {}, 이사 {migrating})", phase_name(phase), phase_name(next), working.len());
        }
        match act {
            None => {}
            Some(Act::Arm) => {
                if let Err(e) = kasa_socket::lid_away::set_sleep_disabled(true) {
                    eprintln!("[lid-away] 잠자기를 못 막았다: {e:#}");
                    self.set_toast(format!("덮어도 마저 하기: 잠자기를 못 막았어요 — {e:#}"));
                    STATE.with(|s| {
                        let mut st = s.borrow_mut();
                        st.phase = Phase::Idle;
                        st.arm_retry_at = Some(now + Duration::from_secs(60));
                    });
                    READY.store(0, Ordering::Release);
                    if let Ok(mut at) = READY_AT.lock() {
                        *at = None;
                    }
                } else {
                    write_heartbeat();
                    STATE.with(|s| {
                        let mut st = s.borrow_mut();
                        st.beat_at = Some(now);
                        hold_app_nap_off(&mut st, true);
                    });
                }
            }
            Some(Act::Disarm) => self.lid_away_release(false),
            Some(Act::Sleep) => self.lid_away_release(true),
        }
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            if st.phase != Phase::Idle && st.beat_at.is_none_or(|t| now.duration_since(t) >= HEARTBEAT_EVERY) {
                write_heartbeat();
                st.beat_at = Some(now);
            }
        });
    }

    /// 이 기기에서 지금 일하는 학생 칸. 거울은 저쪽 기기의 일이다.
    fn lid_away_working_panes(&self) -> Vec<String> {
        let Ok(ws) = self.ws.lock() else { return Vec::new() };
        let mut panes: Vec<String> = self
            .pty
            .keys()
            .filter(|p| !kasa_mcp::remote::is_remote_pane(p))
            .filter(|p| self.pane_agent_working(&ws, p))
            .cloned()
            .collect();
        panes.sort();
        panes
    }

    fn lid_away_release(&mut self, sleep: bool) {
        if let Err(e) = release() {
            eprintln!("[lid-away] 잠자기 막음을 못 풀었다: {e:#}");
            self.set_toast(format!("덮어도 마저 하기: 잠자기 막음을 못 풀었어요 — 터미널에서 sudo pmset -a disablesleep 0 ({e:#})"));
        }
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.beat_at = None;
            hold_app_nap_off(&mut st, false);
        });
        if sleep {
            if let Err(e) = kasa_socket::lid_away::sleep_now() {
                eprintln!("[lid-away] 재우지 못했다: {e:#}");
            }
        }
    }
}

/// 덮이면 창이 안 보여 App Nap 이 타이머를 몇 분씩 미룬다 — 그러면 박동이 끊겨 감시가 막음을 풀고,
/// 일이 끝나도 제때 못 재운다. 깨어 있는 동안만 활동을 쥔다(유휴 잠자기 허락은 pmset 이 따로 쥔다).
fn hold_app_nap_off(st: &mut State, on: bool) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    let Some(cls) = AnyClass::get(c"NSProcessInfo") else { return };
    let info: *mut AnyObject = unsafe { msg_send![cls, processInfo] };
    if info.is_null() {
        return;
    }
    match (on, st.activity.take()) {
        (true, Some(held)) => st.activity = Some(held),
        (true, None) => {
            // NSActivityUserInitiatedAllowingIdleSystemSleep
            let options: u64 = 0x00FF_FFFF & !(1u64 << 20);
            let reason = objc2_foundation::NSString::from_str("kasaterm lid-away");
            st.activity = unsafe { msg_send![info, beginActivityWithOptions: options, reason: &*reason] };
        }
        (false, Some(held)) => {
            let _: () = unsafe { msg_send![info, endActivity: &*held] };
        }
        (false, None) => {}
    }
}

mod probe {
    use std::ffi::{c_char, c_void};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IORegistryEntryCreateCFProperty(entry: u32, key: *const c_void, allocator: *const c_void, options: u32) -> *const c_void;
        fn IOObjectRelease(object: u32) -> i32;
        fn IOPSCopyPowerSourcesInfo() -> *const c_void;
        fn IOPSCopyPowerSourcesList(blob: *const c_void) -> *const c_void;
        fn IOPSGetPowerSourceDescription(blob: *const c_void, source: *const c_void) -> *const c_void;
        fn IOPSGetProvidingPowerSourceType(blob: *const c_void) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
        fn CFStringCompare(a: *const c_void, b: *const c_void, options: usize) -> isize;
        fn CFRelease(cf: *const c_void);
        fn CFGetTypeID(cf: *const c_void) -> usize;
        fn CFBooleanGetTypeID() -> usize;
        fn CFBooleanGetValue(b: *const c_void) -> u8;
        fn CFArrayGetCount(a: *const c_void) -> isize;
        fn CFArrayGetValueAtIndex(a: *const c_void, i: isize) -> *const c_void;
        fn CFDictionaryGetValue(d: *const c_void, key: *const c_void) -> *const c_void;
        fn CFNumberGetValue(n: *const c_void, kind: isize, out: *mut c_void) -> u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGDisplayIsBuiltin(display: u32) -> u32;
    }

    const UTF8: u32 = 0x0800_0100;
    const SINT32: isize = 3;

    struct CfString(*const c_void);
    impl CfString {
        fn new(s: &std::ffi::CStr) -> Self {
            Self(unsafe { CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), UTF8) })
        }
    }
    impl Drop for CfString {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }

    /// 리그는 파일 하나로 덮개를 흉내 낸다(`closed` 면 덮임).
    pub(super) fn lid_closed() -> bool {
        if let Some(path) = std::env::var_os("KASATERM_LID_PROBE").filter(|v| !v.is_empty()) {
            return std::fs::read_to_string(path).is_ok_and(|s| s.trim() == "closed");
        }
        unsafe {
            let root = IOServiceGetMatchingService(0, IOServiceMatching(c"IOPMrootDomain".as_ptr()));
            if root == 0 {
                return false;
            }
            let key = CfString::new(c"AppleClamshellState");
            let value = IORegistryEntryCreateCFProperty(root, key.0, std::ptr::null(), 0);
            IOObjectRelease(root);
            if value.is_null() {
                return false;
            }
            let closed = CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value) != 0;
            CFRelease(value);
            closed
        }
    }

    /// 외부 모니터를 꽂고 덮은 채 쓰는 자리 — 사람이 앞에 있다.
    pub(super) fn external_display() -> bool {
        if std::env::var_os("KASATERM_LID_PROBE").is_some_and(|v| !v.is_empty()) {
            return false;
        }
        let mut ids = [0u32; 16];
        let mut count = 0u32;
        if unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) } != 0 {
            return false;
        }
        ids[..count as usize].iter().any(|id| unsafe { CGDisplayIsBuiltin(*id) } == 0)
    }

    /// 전원에 꽂혀 있거나 배터리가 `floor`% 이상.
    pub(super) fn power_ok(floor: u8) -> bool {
        unsafe {
            let blob = IOPSCopyPowerSourcesInfo();
            if blob.is_null() {
                return true;
            }
            let ac = CfString::new(c"AC Power");
            let providing = IOPSGetProvidingPowerSourceType(blob);
            let mut ok = !providing.is_null() && CFStringCompare(providing, ac.0, 0) == 0;
            if !ok {
                let list = IOPSCopyPowerSourcesList(blob);
                if list.is_null() {
                    ok = true;
                } else {
                    let cur_key = CfString::new(c"Current Capacity");
                    let max_key = CfString::new(c"Max Capacity");
                    for i in 0..CFArrayGetCount(list) {
                        let desc = IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(list, i));
                        if desc.is_null() {
                            continue;
                        }
                        let (mut cur, mut max) = (0i32, 0i32);
                        let cur_v = CFDictionaryGetValue(desc, cur_key.0);
                        let max_v = CFDictionaryGetValue(desc, max_key.0);
                        if !cur_v.is_null()
                            && !max_v.is_null()
                            && CFNumberGetValue(cur_v, SINT32, &mut cur as *mut i32 as *mut c_void) != 0
                            && CFNumberGetValue(max_v, SINT32, &mut max as *mut i32 as *mut c_void) != 0
                            && max > 0
                        {
                            ok = cur * 100 / max >= floor as i32;
                            break;
                        }
                    }
                    CFRelease(list);
                }
            }
            CFRelease(blob);
            ok
        }
    }

    /// 가방 속에서 달아오르면 일보다 기계가 먼저다.
    pub(super) fn thermal_serious() -> bool {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let Some(cls) = AnyClass::get(c"NSProcessInfo") else { return false };
        let info: *mut AnyObject = unsafe { msg_send![cls, processInfo] };
        if info.is_null() {
            return false;
        }
        // NSProcessInfoThermalStateSerious = 2
        let state: isize = unsafe { msg_send![info, thermalState] };
        state >= 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sense(now: Instant) -> Sense {
        Sense { allowed: true, busy: false, departed: false, cap: Duration::from_secs(30 * 60), now }
    }

    #[test]
    fn arms_only_when_work_needs_it_and_sudo_allows() {
        let t = Instant::now();
        assert_eq!(step(Phase::Idle, sense(t)), (Phase::Idle, None));
        let busy = Sense { busy: true, ..sense(t) };
        assert_eq!(step(Phase::Idle, busy), (Phase::Armed { quiet_since: None }, Some(Act::Arm)));
        assert_eq!(step(Phase::Idle, Sense { allowed: false, ..busy }), (Phase::Idle, None));
    }

    #[test]
    fn a_dark_wake_with_the_lid_closed_does_not_arm_again() {
        let s = Sense { busy: true, departed: true, ..sense(Instant::now()) };
        assert_eq!(step(Phase::Idle, s), (Phase::Idle, None));
    }

    #[test]
    fn a_short_pause_between_turns_keeps_the_mac_awake() {
        let t = Instant::now();
        let (p, act) = step(Phase::Armed { quiet_since: None }, sense(t));
        assert_eq!(act, None);
        let (p, act) = step(p, sense(t + QUIET_GRACE - Duration::from_secs(1)));
        assert_eq!(act, None);
        let (p, _) = step(p, Sense { busy: true, ..sense(t + QUIET_GRACE) });
        assert_eq!(p, Phase::Armed { quiet_since: None });
        let (p, _) = step(p, sense(t + QUIET_GRACE * 2));
        assert_eq!(step(p, sense(t + QUIET_GRACE * 3 + Duration::from_secs(1))), (Phase::Idle, Some(Act::Disarm)));
    }

    #[test]
    fn closing_the_lid_waits_for_the_work_to_finish_then_sleeps() {
        let t = Instant::now();
        let closed = |d: u64, busy| Sense { departed: true, busy, ..sense(t + Duration::from_secs(d)) };
        let (p, act) = step(Phase::Armed { quiet_since: None }, closed(0, true));
        assert_eq!((p, act), (Phase::Away { since: t, quiet_since: None }, None), "덮여도 옮기지 않고 깨어 있는다");
        let (p, act) = step(p, closed(600, true));
        assert_eq!(act, None, "일하는 동안은 계속 깨어 있다");
        let (p, act) = step(p, closed(700, false));
        assert_eq!(act, None, "턴이 막 끝났으면 다음 쪽지를 잠깐 기다린다");
        let (p, act) = step(p, closed(730, true));
        assert_eq!((p, act), (Phase::Away { since: t, quiet_since: None }, None), "그 사이 다시 일하면 처음부터 기다린다");
        let (p, _) = step(p, closed(800, false));
        assert_eq!(step(p, closed(800 + QUIET_GRACE.as_secs(), false)), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn work_that_already_ended_before_the_lid_closed_sleeps_after_the_rest_of_the_pause() {
        let t = Instant::now();
        let (p, _) = step(Phase::Armed { quiet_since: None }, sense(t));
        let (p, act) = step(p, Sense { departed: true, ..sense(t + Duration::from_secs(50)) });
        assert_eq!(act, None);
        assert_eq!(step(p, Sense { departed: true, ..sense(t + QUIET_GRACE) }), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn opening_the_lid_keeps_the_block_and_stops_counting_toward_sleep() {
        let t = Instant::now();
        let away = Phase::Away { since: t, quiet_since: None };
        assert_eq!(step(away, Sense { busy: true, ..sense(t) }), (Phase::Armed { quiet_since: None }, None));
    }

    #[test]
    fn the_cap_sleeps_even_while_students_still_work() {
        let t = Instant::now();
        let away = Phase::Away { since: t, quiet_since: None };
        let at = |d: Duration| Sense { departed: true, busy: true, ..sense(t + d) };
        let cap = Duration::from_secs(30 * 60);
        assert_eq!(step(away, at(cap - Duration::from_secs(1))).1, None);
        assert_eq!(step(away, at(cap)), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn low_battery_or_heat_at_the_lid_goes_straight_to_sleep() {
        let t = Instant::now();
        let s = Sense { allowed: false, departed: true, busy: true, ..sense(t) };
        assert_eq!(step(Phase::Armed { quiet_since: None }, s), (Phase::Idle, Some(Act::Sleep)));
        assert_eq!(step(Phase::Away { since: t, quiet_since: None }, s), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn a_machine_chosen_for_the_old_move_keeps_the_feature_on() {
        let on = |v: serde_json::Value| config_from(&v).on;
        assert!(on(serde_json::json!({ "lid_away_machine": "나쵸네코" })));
        assert!(!on(serde_json::json!({ "lid_away_machine": "" })));
        assert!(!on(serde_json::json!({})));
        assert!(!on(serde_json::json!({ "lid_away": false, "lid_away_machine": "나쵸네코" })), "새 값이 이긴다");
        assert_eq!(config_from(&serde_json::json!({ "lid_away": true })).cap, Duration::from_secs(DEFAULT_MINUTES * 60));
    }

    #[test]
    fn probes_answer_without_crashing() {
        let _ = probe::lid_closed();
        let _ = probe::external_display();
        let _ = probe::power_ok(BATTERY_FLOOR);
        let _ = probe::thermal_serious();
    }
}
