//! 프레임을 화면 주사율보다 자주 내지 않는다.
//!
//! PTY 출력이 오면 그 자리에서 그린다(`handler.rs` `user_event` 끝 — 메아리가 한 바퀴 늦지
//! 않게). 창이 많아 출력이 초당 수백 번 오면 그림도 수백 장이 되는데, 화면은 주사율만큼만
//! 받아 가므로 남는 그림은 CAMetalLayer 가 다음 그림판을 내줄 때까지 메인 스레드를 재운다.
//! 창 26개 리그에서 메인 스레드 시간의 31%가 그 대기였고, 그동안 클릭은 줄 서서 기다렸다
//! (2026-10-07 실측, 「창이 많으면 누를 때 멈칫한다」).
//!
//! 그래서 직전 그림이 한 주기 안이면 이번 것은 주기 끝으로 미뤄 한 장으로 합친다. 첫 변화는
//! 여전히 그 자리에서 그려지므로 타자 메아리는 늦지 않는다. 미룬 그림은 따로 깨워 반드시
//! 그린다 — 출력이 거기서 끊겨도 마지막 화면이 낡은 채 남지 않게.
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

struct Pace {
    last: Option<Instant>,
    due: Option<Instant>,
    interval: Duration,
    measured: Option<Instant>,
}

static PACE: Mutex<Pace> = Mutex::new(Pace {
    last: None,
    due: None,
    interval: Duration::from_micros(16_667),
    measured: None,
});
static WAKE: Condvar = Condvar::new();
static WAKER: OnceLock<()> = OnceLock::new();

/// 창이 다른 모니터로 옮겨 가면 주사율이 바뀐다 — 이 정도 박자로 다시 읽으면 충분하다.
const REMEASURE: Duration = Duration::from_secs(2);

/// 화면 주사율까지 그린다. Windows 는 한때 60 으로 묶었다 — vsync 없이 내서(`AutoNoVsync`) 출력이 이어지면 143Hz
/// 만큼 전체를 다시 그렸고, 한 장마다 사이드바가 프로세스 표를 훑어 메인 스레드가 한 코어의 70%를 썼다
/// (2026-11-13 집 PC). 그 뒤 기기 목록·거울 투영·쉬는 칸 합성을 다시 쓰게 되어 3840x1600 한 장이 3~5ms 라,
/// 144Hz 화면에서 거울이 60장에 묶여 끊겨 보이던 것을 푼다.
const MAX_HZ: f64 = 240.0;

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
