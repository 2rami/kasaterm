//! 이벤트 루프 한 바퀴(`new_events` → `about_to_wait` 끝)에 메인 스레드가 얼마나 묶였나.
//!
//! macOS 는 메인 스레드가 이벤트를 2초 남짓 못 받으면 무지개 커서를 띄운다. 그 직전 단계가
//! 「눌렀는데 멈칫」이라, 클릭이 든 바퀴의 길이가 곧 사람이 느끼는 반응이다. 창이 많을 때
//! 느려진다는 말을 숫자로 가르려면 이 길이가 있어야 한다(2026-10-07).
//!
//! 1초를 넘긴 바퀴는 늘 `[stall]` 로 남긴다 — 다른 기기의 앱 로그(`remote-bake.sh applog`)로
//! 무엇이 멈췄는지 볼 수 있게. `KASATERM_PROFILE` 이면 클릭이 든 바퀴와 50ms 넘는 바퀴를 모두 찍는다.
use std::cell::Cell;
use std::time::{Duration, Instant};

const STALL: Duration = Duration::from_secs(1);
const SLOW: Duration = Duration::from_millis(50);

thread_local! {
    static TURN: Cell<Option<Instant>> = const { Cell::new(None) };
    static CLICKED: Cell<bool> = const { Cell::new(false) };
    static PROFILE: bool = std::env::var_os("KASATERM_PROFILE").is_some();
}

pub(crate) fn begin() {
    TURN.with(|t| {
        if t.get().is_none() {
            t.set(Some(Instant::now()));
            crate::stall_dump::turn_opened();
        }
    });
}

pub(crate) fn note_click() {
    CLICKED.with(|c| c.set(true));
}

/// `about_to_wait` 첫 줄에서 잡아 두면, 어느 갈래로 빠져나가든 떨어질 때 바퀴를 닫는다.
pub(crate) struct TurnEnd;

impl Drop for TurnEnd {
    fn drop(&mut self) {
        let Some(start) = TURN.with(|t| t.take()) else { return };
        crate::stall_dump::turn_closed();
        let clicked = CLICKED.with(|c| c.replace(false));
        let took = start.elapsed();
        if took >= STALL {
            eprintln!("[stall] main thread {}ms{}", took.as_millis(), if clicked { " (click)" } else { "" });
        } else if PROFILE.with(|p| *p) && (clicked || took >= SLOW) {
            eprintln!("[turn] {} {}ms", if clicked { "click" } else { "slow" }, took.as_millis());
        }
    }
}

/// `KASATERM_PROFILE` 이면 한 장을 구간(칸 합성·합성 뒤 준비·크롬과 셀·GPU 제출)으로 나눠 재고 1초마다
/// `[frame]` 으로 p50·p95 를 찍는다. 장 전체 길이(`[render-gpu]`)만으로는 어느 구간을 줄여야 하는지 모른다.
const FRAME_PHASES: [&str; 4] = ["panes", "prep", "chrome", "gpu"];

#[derive(Default)]
struct FrameClock {
    last: Option<Instant>,
    current: [u32; 4],
    window: Vec<[u32; 4]>,
    since: Option<Instant>,
}

thread_local! {
    static FRAME: std::cell::RefCell<FrameClock> = std::cell::RefCell::new(FrameClock::default());
}

pub(crate) fn frame_begin() {
    if !PROFILE.with(|p| *p) {
        return;
    }
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        f.last = Some(Instant::now());
        f.current = [0; 4];
    });
}

pub(crate) fn frame_mark(phase: usize) {
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        let Some(last) = f.last else { return };
        let now = Instant::now();
        f.current[phase] = now.duration_since(last).as_micros() as u32;
        f.last = Some(now);
    });
}

pub(crate) fn frame_end() {
    FRAME.with(|f| {
        let mut f = f.borrow_mut();
        if f.last.take().is_none() {
            return;
        }
        let sample = f.current;
        f.window.push(sample);
        let since = *f.since.get_or_insert_with(Instant::now);
        if since.elapsed() < Duration::from_secs(1) {
            return;
        }
        let window = std::mem::take(&mut f.window);
        f.since = Some(Instant::now());
        let pick = |mut v: Vec<u32>, q: f32| {
            v.sort_unstable();
            v[((v.len() - 1) as f32 * q) as usize]
        };
        let totals: Vec<u32> = window.iter().map(|s| s.iter().sum()).collect();
        let mut line = format!("[frame] n={} total p50={}us p95={}us", window.len(), pick(totals.clone(), 0.5), pick(totals, 0.95));
        for (i, name) in FRAME_PHASES.iter().enumerate() {
            let v: Vec<u32> = window.iter().map(|s| s[i]).collect();
            line.push_str(&format!(" | {name} p50={}us p95={}us", pick(v.clone(), 0.5), pick(v, 0.95)));
        }
        eprintln!("{line}");
    });
}

/// `KASATERM_LAG_PROBE` — 25ms 마다 꼬리표 붙은 `Redraw` 를 보내, 메인 스레드가 받기까지
/// 걸린 시간을 1초마다 `[lag]` 로 찍는다. 바퀴 길이는 처리 시간만 말하고, 사람이 누른 클릭이
/// 처리되기 **전에** 줄 서서 기다린 시간은 이것만 보인다. 한 번에 꼬리표 하나만 띄운다.
static LAG_SENT: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

pub(crate) fn start_lag_probe(proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    if std::env::var_os("KASATERM_LAG_PROBE").is_none() {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(25));
        let mut sent = LAG_SENT.lock().unwrap();
        if sent.is_none() {
            *sent = Some(Instant::now());
            drop(sent);
            if proxy.send_event(crate::UserEvent::Redraw).is_err() {
                return;
            }
        }
    });
}

/// `Redraw` 를 받을 때마다 부른다. 꼬리표 이전에 쌓인 `Redraw` 가 먼저 꺼내도 그 순간까지
/// 메인 스레드가 이벤트를 못 받은 것은 같으니, 재려는 값(못 받은 시간)은 그대로다.
pub(crate) fn lag_received() {
    let Some(at) = LAG_SENT.lock().unwrap().take() else { return };
    thread_local! {
        static WIN: std::cell::RefCell<(Vec<u32>, Option<Instant>)> = const { std::cell::RefCell::new((Vec::new(), None)) };
    }
    WIN.with(|w| {
        let mut w = w.borrow_mut();
        w.0.push(at.elapsed().as_millis() as u32);
        let start = *w.1.get_or_insert_with(Instant::now);
        if start.elapsed() >= Duration::from_secs(1) {
            let mut v = std::mem::take(&mut w.0);
            v.sort_unstable();
            let pick = |q: f32| v[((v.len() - 1) as f32 * q) as usize];
            eprintln!("[lag] n={} p50={}ms p90={}ms max={}ms", v.len(), pick(0.5), pick(0.9), pick(1.0));
            w.1 = Some(Instant::now());
        }
    });
}
