//! 덮으면 이사 — 학생이 일하는 동안 잠자기를 막아 두고(`Armed`), 덮이면 일하는 학생을 고른 기기로
//! 예약 이사시킨 뒤(`Away`) 다 옮겨지거나 상한이 차면 재운다. 다시 열면 남은 예약을 거둔다.
//! 설치물·pmset 명령은 `kasa_socket::lid_away`, 이사 자체는 `session/migrate.rs` 의 예약 경로다.
//!
//! 쉬는 학생은 옮기지 않는다 — 이어서 할 일이 없고, 다시 열었을 때 칸이 통째로 다른 기기 거울로
//! 바뀌어 있으면 손해가 더 크다. 외부 모니터를 꽂고 덮은 채 쓰는 자리는 떠난 것이 아니다.
use super::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
/// 턴 사이 잠깐 쉬는 동안 막음을 풀었다 다시 거는 일을 줄인다.
const QUIET_GRACE: Duration = Duration::from_secs(60);
/// 이사가 끝나도 바로 재우지 않는다 — 저쪽 셸에 `--resume` 을 넣는 마지막 바이트가 건너가야 한다.
const SETTLE: Duration = Duration::from_secs(15);
/// 상한이 찬 뒤에도 도는 이사는 마저 끝내게 기다리되, 워커가 매달려도 이 이상은 안 깨어 있는다.
const HARD_EXTRA: Duration = Duration::from_secs(5 * 60);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);
const BATTERY_FLOOR: u8 = 20;
pub(crate) const DEFAULT_MINUTES: u64 = 30;
pub(crate) const MINUTE_CHOICES: [u64; 3] = [10, 30, 60];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    Armed { quiet_since: Option<Instant> },
    Away { since: Instant, stopping: bool, settled_since: Option<Instant> },
}

#[derive(Clone, Copy, Debug)]
struct Sense {
    /// 켜져 있고, sudo 가 허락하고, 전원·열이 괜찮다.
    allowed: bool,
    working: bool,
    /// 덮였고 외부 모니터도 없다.
    departed: bool,
    migrating: bool,
    cap: Duration,
    now: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Arm,
    Disarm,
    /// 일하는 학생 중 아직 안 맡은 칸을 예약 이사에 건다.
    Depart,
    /// 다시 열렸다 — 아직 안 떠난 예약을 거둔다.
    Recall,
    /// 상한·전원·열 — 새 예약을 멈추고 안 떠난 예약을 거둔다. 도는 이사는 마저 끝낸다.
    Halt,
    Sleep,
}

fn step(phase: Phase, s: Sense) -> (Phase, Option<Act>) {
    match phase {
        Phase::Idle => {
            // 덮인 채 잠깐 깬 것(Power Nap)에 다시 걸면 가방 속에서 상한만큼 깨어 있게 된다.
            if s.allowed && (s.working || s.migrating) && !s.departed {
                (Phase::Armed { quiet_since: None }, Some(Act::Arm))
            } else {
                (Phase::Idle, None)
            }
        }
        Phase::Armed { quiet_since } => {
            if s.departed {
                return if s.allowed {
                    (Phase::Away { since: s.now, stopping: false, settled_since: None }, Some(Act::Depart))
                } else {
                    (Phase::Idle, Some(Act::Sleep))
                };
            }
            if !s.allowed {
                return (Phase::Idle, Some(Act::Disarm));
            }
            if s.working || s.migrating {
                return (Phase::Armed { quiet_since: None }, None);
            }
            let since = quiet_since.unwrap_or(s.now);
            if s.now.duration_since(since) >= QUIET_GRACE {
                (Phase::Idle, Some(Act::Disarm))
            } else {
                (Phase::Armed { quiet_since: Some(since) }, None)
            }
        }
        Phase::Away { since, stopping, settled_since } => {
            if !s.departed {
                return (Phase::Armed { quiet_since: None }, Some(Act::Recall));
            }
            let away_for = s.now.duration_since(since);
            if !stopping && (away_for >= s.cap || !s.allowed) {
                return (Phase::Away { since, stopping: true, settled_since: None }, Some(Act::Halt));
            }
            if stopping && away_for >= s.cap + HARD_EXTRA {
                return (Phase::Idle, Some(Act::Sleep));
            }
            // 멈춘 뒤에는 일하는 학생을 더 기다리지 않는다 — 그게 상한의 뜻이다.
            let busy = s.migrating || (!stopping && s.working);
            let next_act = (!stopping).then_some(Act::Depart);
            if busy {
                return (Phase::Away { since, stopping, settled_since: None }, next_act);
            }
            let settled = settled_since.unwrap_or(s.now);
            if s.now.duration_since(settled) >= SETTLE {
                (Phase::Idle, Some(Act::Sleep))
            } else {
                (Phase::Away { since, stopping, settled_since: Some(settled) }, next_act)
            }
        }
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "Idle",
        Phase::Armed { .. } => "Armed",
        Phase::Away { stopping: false, .. } => "Away",
        Phase::Away { stopping: true, .. } => "Away(멈춤)",
    }
}

#[derive(Clone)]
struct Config {
    machine: String,
    cap: Duration,
}

struct State {
    phase: Phase,
    booted: bool,
    last_tick: Option<Instant>,
    beat_at: Option<Instant>,
    targets: HashSet<String>,
    skipped: HashSet<String>,
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
            targets: HashSet::new(),
            skipped: HashSet::new(),
            arm_retry_at: None,
            config: None,
            safe: None,
            activity: None,
        })
    };
}

fn read_config() -> Config {
    let settings = socket::read_settings();
    let machine = settings["lid_away_machine"].as_str().unwrap_or_default().trim().to_string();
    let minutes = settings["lid_away_minutes"].as_u64().filter(|m| *m > 0).unwrap_or(DEFAULT_MINUTES);
    Config { machine, cap: Duration::from_secs(minutes * 60) }
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

pub(crate) fn chosen_machine() -> String {
    read_config().machine
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
        Phase::Away { .. } => Some("덮인 채 학생을 옮기는 중이에요"),
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
        let machine = (!cfg.machine.is_empty())
            .then(|| kasa_mcp::machines::find(&cfg.machine))
            .flatten();
        if machine.is_none() && phase == Phase::Idle {
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
            allowed: machine.is_some() && sudo_ready() == Some(true) && safe && !arm_blocked,
            working: !working.is_empty(),
            departed,
            migrating,
            cap: cfg.cap,
            now,
        };
        let (next, act) = step(phase, sense);
        STATE.with(|s| s.borrow_mut().phase = next);
        if let Some(a) = act.filter(|a| *a != Act::Depart || phase_name(phase) != phase_name(next)) {
            eprintln!("[lid-away] {a:?} → {} (일하는 칸 {})", phase_name(next), working.len());
        }
        match act {
            None => {}
            Some(Act::Arm) => {
                if let Err(e) = kasa_socket::lid_away::set_sleep_disabled(true) {
                    eprintln!("[lid-away] 잠자기를 못 막았다: {e:#}");
                    self.set_toast(format!("덮으면 이사: 잠자기를 못 막았어요 — {e:#}"));
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
            Some(Act::Depart) => {
                if let Some(m) = &machine {
                    self.lid_away_depart(m, &working);
                }
            }
            Some(Act::Recall) | Some(Act::Halt) => {
                STATE.with(|s| {
                    let mut st = s.borrow_mut();
                    let targets = std::mem::take(&mut st.targets);
                    self.migrate_queue.retain(|q| !targets.contains(&q.pane));
                    st.skipped.clear();
                });
            }
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

    fn lid_away_depart(&mut self, machine: &kasa_mcp::machines::Machine, working: &[String]) {
        let fresh: Vec<String> = STATE.with(|s| {
            let st = s.borrow();
            working
                .iter()
                .filter(|p| !st.targets.contains(*p) && !st.skipped.contains(*p))
                .cloned()
                .collect()
        });
        for pane in fresh {
            let outcome = (|| -> anyhow::Result<String> {
                let local = self
                    .pty
                    .get(&pane)
                    .and_then(|s| s.shell_pid())
                    .and_then(socket::pid_cwd)
                    .ok_or_else(|| anyhow::anyhow!("작업 폴더를 못 읽었다"))?;
                let remote = kasa_mcp::machines::map_local_to_remote(machine, &local.to_string_lossy())
                    .ok_or_else(|| anyhow::anyhow!("{} 를 {} 경로로 못 옮긴다 — machines.json roots", local.display(), machine.label))?;
                self.migrate_pane(&pane, &machine.base, Some(&remote), false, None)
            })();
            STATE.with(|s| {
                let mut st = s.borrow_mut();
                match &outcome {
                    Ok(msg) => {
                        eprintln!("[lid-away] {pane} → {}: {msg}", machine.label);
                        st.targets.insert(pane.clone());
                    }
                    Err(e) => {
                        eprintln!("[lid-away] {pane} 는 남는다: {e:#}");
                        st.skipped.insert(pane.clone());
                    }
                }
            });
        }
    }

    fn lid_away_release(&mut self, sleep: bool) {
        if let Err(e) = release() {
            eprintln!("[lid-away] 잠자기 막음을 못 풀었다: {e:#}");
            self.set_toast(format!("덮으면 이사: 잠자기 막음을 못 풀었어요 — 터미널에서 sudo pmset -a disablesleep 0 ({e:#})"));
        }
        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.targets.clear();
            st.skipped.clear();
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
/// 예약 이사도 제때 안 뜬다. 깨어 있는 동안만 활동을 쥔다(유휴 잠자기 허락은 pmset 이 따로 쥔다).
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
        Sense {
            allowed: true,
            working: false,
            departed: false,
            migrating: false,
            cap: Duration::from_secs(30 * 60),
            now,
        }
    }

    #[test]
    fn arms_only_when_work_or_migration_needs_it_and_sudo_allows() {
        let t = Instant::now();
        assert_eq!(step(Phase::Idle, sense(t)), (Phase::Idle, None));
        let working = Sense { working: true, ..sense(t) };
        assert_eq!(step(Phase::Idle, working), (Phase::Armed { quiet_since: None }, Some(Act::Arm)));
        assert_eq!(step(Phase::Idle, Sense { allowed: false, ..working }), (Phase::Idle, None));
    }

    #[test]
    fn a_self_requested_move_keeps_sleep_blocked_between_turns() {
        let t = Instant::now();
        let moving = Sense { migrating: true, ..sense(t) };
        let (armed, act) = step(Phase::Idle, moving);
        assert_eq!(act, Some(Act::Arm));
        let (armed, act) = step(armed, Sense { now: t + QUIET_GRACE * 2, ..moving });
        assert_eq!(act, None);
        assert_eq!(armed, Phase::Armed { quiet_since: None });
        let closed = Sense { departed: true, now: t + QUIET_GRACE * 3, ..moving };
        let (away, act) = step(armed, closed);
        assert_eq!(act, Some(Act::Depart));
        assert_eq!(step(away, Sense { now: closed.now + SETTLE * 2, ..closed }).1, Some(Act::Depart));
        assert!(matches!(step(away, Sense { now: closed.now + SETTLE * 2, ..closed }).0,
            Phase::Away { settled_since: None, .. }));
        assert_eq!(step(Phase::Idle, Sense { allowed: false, ..moving }), (Phase::Idle, None));
        assert_eq!(step(Phase::Idle, closed), (Phase::Idle, None));
    }

    #[test]
    fn a_dark_wake_with_the_lid_closed_does_not_arm_again() {
        let s = Sense { working: true, departed: true, ..sense(Instant::now()) };
        assert_eq!(step(Phase::Idle, s), (Phase::Idle, None));
    }

    #[test]
    fn a_short_pause_between_turns_keeps_the_mac_awake() {
        let t = Instant::now();
        let (p, act) = step(Phase::Armed { quiet_since: None }, sense(t));
        assert_eq!(act, None);
        let (p, act) = step(p, sense(t + QUIET_GRACE - Duration::from_secs(1)));
        assert_eq!(act, None);
        let (p, _) = step(p, Sense { working: true, ..sense(t + QUIET_GRACE) });
        assert_eq!(p, Phase::Armed { quiet_since: None });
        let (p, _) = step(p, sense(t + QUIET_GRACE * 2));
        assert_eq!(step(p, sense(t + QUIET_GRACE * 3 + Duration::from_secs(1))), (Phase::Idle, Some(Act::Disarm)));
    }

    #[test]
    fn closing_the_lid_reserves_moves_and_sleeps_once_they_land() {
        let t = Instant::now();
        let closed = |now, working, migrating| Sense { departed: true, working, migrating, ..sense(now) };
        let (p, act) = step(Phase::Armed { quiet_since: None }, closed(t, true, false));
        assert_eq!(act, Some(Act::Depart));
        let (p, act) = step(p, closed(t + Duration::from_secs(60), true, true));
        assert_eq!(act, Some(Act::Depart));
        let (p, act) = step(p, closed(t + Duration::from_secs(120), false, false));
        assert_eq!(act, Some(Act::Depart));
        assert!(matches!(p, Phase::Away { settled_since: Some(_), .. }));
        assert_eq!(step(p, closed(t + Duration::from_secs(120) + SETTLE, false, false)), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn opening_the_lid_takes_back_the_reservations() {
        let t = Instant::now();
        let away = Phase::Away { since: t, stopping: false, settled_since: None };
        assert_eq!(step(away, Sense { working: true, ..sense(t) }), (Phase::Armed { quiet_since: None }, Some(Act::Recall)));
    }

    #[test]
    fn the_cap_stops_waiting_for_work_but_lets_a_move_finish() {
        let t = Instant::now();
        let away = Phase::Away { since: t, stopping: false, settled_since: None };
        let at = |d: Duration, working, migrating| Sense { departed: true, working, migrating, ..sense(t + d) };
        let cap = Duration::from_secs(30 * 60);
        let (p, act) = step(away, at(cap, true, true));
        assert_eq!(act, Some(Act::Halt));
        let (p, act) = step(p, at(cap + Duration::from_secs(10), true, true));
        assert_eq!(act, None);
        // 이사가 끝나면 아직 일하는 학생이 있어도 잔다.
        let (p, _) = step(p, at(cap + Duration::from_secs(20), true, false));
        assert_eq!(step(p, at(cap + Duration::from_secs(20) + SETTLE, true, false)), (Phase::Idle, Some(Act::Sleep)));
        // 워커가 매달려도 상한 너머로는 안 버틴다.
        let stuck = Phase::Away { since: t, stopping: true, settled_since: None };
        assert_eq!(step(stuck, at(cap + HARD_EXTRA, true, true)), (Phase::Idle, Some(Act::Sleep)));
    }

    #[test]
    fn low_battery_or_heat_at_the_lid_goes_straight_to_sleep() {
        let t = Instant::now();
        let s = Sense { allowed: false, departed: true, working: true, ..sense(t) };
        assert_eq!(step(Phase::Armed { quiet_since: None }, s), (Phase::Idle, Some(Act::Sleep)));
        let away = Phase::Away { since: t, stopping: false, settled_since: None };
        assert_eq!(step(away, s).1, Some(Act::Halt));
    }

    #[test]
    fn probes_answer_without_crashing() {
        let _ = probe::lid_closed();
        let _ = probe::external_display();
        let _ = probe::power_ok(BATTERY_FLOOR);
        let _ = probe::thermal_serious();
    }
}
