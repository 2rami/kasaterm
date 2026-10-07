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

fn refresh_interval(window: &Window) -> Duration {
    let hz = window
        .current_monitor()
        .and_then(|m| m.refresh_rate_millihertz())
        .map_or(60.0, |mhz| f64::from(mhz) / 1000.0)
        .clamp(30.0, 240.0);
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
                    p = WAKE.wait_timeout(p, due - now).unwrap().0;
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
