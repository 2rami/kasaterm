//! 프레임을 화면 주사율보다 자주 내지 않는다.
//!
//! PTY 출력이 오면 그 자리에서 그린다(`handler.rs` `user_event` 끝 — 메아리가 한 바퀴 늦지
//! 않게). 창이 많아 출력이 초당 수백 번 오면 그림도 수백 장이 되는데, 화면은 주사율만큼만
//! 받아 가므로 남는 그림은 CAMetalLayer 가 다음 그림판을 내줄 때까지 메인 스레드를 재운다.
//! 창 26개 리그에서 메인 스레드 시간의 31%가 그 대기였고, 그동안 클릭은 줄 서서 기다렸다
//! (2026-10-07 실측, 「창이 많으면 누를 때 멈칫한다」).
//!
//! 그래서 직전 그림이 한 주기 안이면 이번 것은 주기 끝으로 미뤄 한 장으로 합친다. 미룬 그림은
//! 따로 깨워 반드시 그린다 — 출력이 거기서 끊겨도 마지막 화면이 낡은 채 남지 않게.
//!
//! 사람이 기다리는 그림(방금 키를 보낸 칸의 새 화면·조합 중 글자)은 미루지 않는다. 바쁜 거울이
//! 여럿이면 직전 그림이 거의 늘 한 주기 안이라 메아리가 담긴 그림의 절반쯤이 미뤄졌다 — 거울 15칸 중
//! 3칸이 바쁜 리그에서 받음→그림 시작 p90 4.4→0.8ms, 조합 글자 이벤트→그림 p90 4→0.2ms(2026-11-18).
//! 키 몇 번 몫의 그림을 더 그리는 값은 작다.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

struct Pace {
    last: Option<Instant>,
    due: Option<Instant>,
    interval: Duration,
    measured: Option<Instant>,
    urgent: Option<Instant>,
    /// 루프를 깨우지 않고 넘긴 화면 갱신의 가장 늦은 시각 — 잡아 둔 장이 실어 간다.
    unwoken: Option<Instant>,
}

static PACE: Mutex<Pace> = Mutex::new(Pace {
    last: None,
    due: None,
    interval: Duration::from_micros(16_667),
    measured: None,
    urgent: None,
    unwoken: None,
});
/// 사람이 키를 보낸 칸과 그 시각.
static TYPED: Mutex<Option<(String, Instant)>> = Mutex::new(None);
/// 화면 갱신이 보낸 깨움이 아직 루프에 안 닿았다 — 그 사이의 갱신은 같은 깨움을 탄다.
static WAKE_POSTED: AtomicBool = AtomicBool::new(false);
static WAKE: Condvar = Condvar::new();
static WAKER: OnceLock<()> = OnceLock::new();

/// 창이 다른 모니터로 옮겨 가면 주사율이 바뀐다 — 이 정도 박자로 다시 읽으면 충분하다.
const REMEASURE: Duration = Duration::from_secs(2);

/// 화면 주사율까지 그린다. Windows 는 한때 60 으로 묶었다 — vsync 없이 내서(`AutoNoVsync`) 출력이 이어지면 143Hz
/// 만큼 전체를 다시 그렸고, 한 장마다 사이드바가 프로세스 표를 훑어 메인 스레드가 한 코어의 70%를 썼다
/// (2026-11-13 집 PC). 그 뒤 기기 목록·거울 투영·쉬는 칸 합성을 다시 쓰게 되어 3840x1600 한 장이 3~5ms 라,
/// 144Hz 화면에서 거울이 60장에 묶여 끊겨 보이던 것을 푼다.
const MAX_HZ: f64 = 240.0;

/// 키를 보낸 뒤 이만큼은 그 칸의 새 화면을 메아리로 본다. 첫 갱신 하나만 보면 일하는 Claude 칸의
/// 스피너 갱신이 먼저 와 정작 메아리는 미뤄진다. 카사넷 왕복 p90(30ms 안팎)을 넉넉히 덮는다.
const ECHO_WINDOW: Duration = Duration::from_millis(250);

fn refresh_interval(window: &Window) -> Duration {
    let hz = window
        .current_monitor()
        .and_then(|m| m.refresh_rate_millihertz())
        .map_or(60.0, |mhz| f64::from(mhz) / 1000.0)
        .clamp(30.0, MAX_HZ);
    Duration::from_secs_f64(1.0 / hz)
}

/// 이번 그림을 미룰까. `true` 면 부른 쪽은 아무것도 그리지 않고 돌아간다 — 미룬 그림은
/// 주기 끝에 `UserEvent::Redraw` 로 다시 온다.
pub(crate) fn hold(window: &Window, proxy: &EventLoopProxy<crate::UserEvent>) -> bool {
    let now = Instant::now();
    let mut p = PACE.lock().unwrap();
    if p.measured.is_none_or(|at| now.duration_since(at) >= REMEASURE) {
        p.interval = refresh_interval(window);
        p.measured = Some(now);
    }
    if p.urgent.is_some() {
        return false;
    }
    let Some(last) = p.last else { return false };
    let due = last + p.interval;
    if now >= due {
        return false;
    }
    if p.due.is_none() {
        p.due = Some(due);
        WAKER.get_or_init(|| spawn_waker(proxy.clone()));
        WAKE.notify_one();
    }
    true
}

/// GPU 로 한 장을 냈다. 주기는 그리기 **시작** 시각에서 잰다 — 끝에서 재면 한 장이 오래
/// 걸릴수록 다음 장이 그만큼 더 밀린다.
pub(crate) fn drew(started: Instant) {
    let mut p = PACE.lock().unwrap();
    p.last = Some(started);
    p.due = None;
    // 그리는 도중에 생긴 급한 그림은 이 장에 안 실렸다 — 다음 장도 미루지 않게 남긴다.
    if p.urgent.is_some_and(|at| at <= started) {
        p.urgent = None;
    }
    // 그리는 도중에 깨우지 않고 넘긴 갱신도 이 장에 안 실렸을 수 있다 — 다음 주기 끝에 한 장 더 잡는다.
    if p.unwoken.take().is_some_and(|at| at > started) {
        p.due = Some(started + p.interval);
        WAKE.notify_one();
    }
}

/// 다음 한 장은 미루지 않는다 — 사람이 그 그림을 기다린다.
pub(crate) fn urgent() {
    PACE.lock().unwrap().urgent = Some(Instant::now());
}

/// 칸에 키를 보냈다. 그 칸의 다음 화면들은 메아리다.
pub(crate) fn typed(pane: &str) {
    *TYPED.lock().unwrap() = Some((pane.to_owned(), Instant::now()));
}

/// 칸의 화면이 바뀌었다. 루프를 깨워야 하면 `true`. 방금 키를 보낸 칸이면 그 그림은 미루지 않는다.
///
/// 주기 끝에 그릴 장이 이미 잡혀 있거나 앞 갱신의 깨움이 아직 가는 중이면 깨우지 않는다 — 그 장·그 깨움이
/// 이 갱신을 싣는다. 스피너가 도는 거울이 여럿이면 갱신마다 루프가 깨어, 그림은 미뤄진 채 about_to_wait 만
/// 한 바퀴씩 돌았다(거울 26칸 중 4칸이 일하는 리그에서 갱신 초당 119번, 그중 100번이 그랬다, 2026-11-18).
/// 원본은 한 틱을 1ms 안에 서너 번으로 나눠 보내 와서, 장이 잡히기 전의 갱신끼리도 깨움 하나를 나눈다.
pub(crate) fn screen_changed(pane: &str) -> bool {
    let echo = TYPED
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(typed, at)| typed == pane && at.elapsed() < ECHO_WINDOW);
    let now = Instant::now();
    let mut p = PACE.lock().unwrap();
    if echo {
        p.urgent = Some(now);
        return !WAKE_POSTED.swap(true, Ordering::AcqRel);
    }
    if p.due.is_some() && p.urgent.is_none() {
        p.unwoken = Some(now);
        return false;
    }
    !WAKE_POSTED.swap(true, Ordering::AcqRel)
}

/// 루프가 깨움(`UserEvent::Redraw`)을 받았다 — 그리기 전에 부른다. 이 뒤의 화면 갱신은 다시 깨운다.
/// 어떤 깨움이든 지운다: 화면 갱신이 보낸 것이 길을 잃어도 다음 깜빡임이 풀어 준다.
pub(crate) fn wake_received() {
    WAKE_POSTED.store(false, Ordering::Release);
}

fn spawn_waker(proxy: EventLoopProxy<crate::UserEvent>) {
    std::thread::Builder::new()
        .name("frame-pace".into())
        .spawn(move || {
            let mut p = PACE.lock().unwrap();
            loop {
                let Some(due) = p.due else {
                    p = WAKE.wait(p).unwrap();
                    continue;
                };
                let now = Instant::now();
                if now < due {
                    // 조건 변수의 시간 제한은 Windows 에서 프로세스 타이머 해상도에 묶인다 — 아무도 해상도를
                    // 올리지 않은 프로세스는 3ms 를 맡겨도 12ms 뒤에 깨고, 올린 앱 안에서도 1ms 씩 늦었다
                    // (거울 15칸 리그에서 미룬 그림 p50 3.8→2.5ms). 해상도는 드라이버 같은 남의 사정이라
                    // 기대지 않고, 고해상도 타이머로 자는 std 의 sleep 을 쓴다.
                    // 자는 동안 `due` 는 `drew` 가 지우거나 더 늦은 값으로만 바뀌어 일찍 깰 일이 없다.
                    drop(p);
                    std::thread::sleep(due - now);
                    p = PACE.lock().unwrap();
                    continue;
                }
                p.due = None;
                drop(p);
                if proxy.send_event(crate::UserEvent::Redraw).is_err() {
                    return;
                }
                p = PACE.lock().unwrap();
            }
        })
        .expect("frame-pace thread");
}
