//! 버전 확인 — 지금 도는 판과 배포 피드의 최신판을 견준다.
//!
//! 확인은 **계정 메뉴를 열 때만** 돈다. 상시 폴링을 안 하는 이유는 이 값이
//! 급하지 않아서다. 새 판이 나왔다는 사실은 30분 늦게 알아도 잃는 게 없는데,
//! 상시 폴링은 아무도 안 보는 동안에도 바깥으로 요청을 내보낸다.
//!
//! HTTP 는 `curl` 로 낸다 — Windows 쪽 appcast 확인(`win_sparkle`)이 이미 같은
//! 길이고, 이 한 건 때문에 HTTP 클라이언트를 의존성에 들이는 것보다 두 OS 다
//! 기본 탑재인 curl 을 부르는 편이 싸다.
//!
//! 화면은 여기서 안 그린다. 결과를 읽는 자리는 상태줄 버전 조각과 계정 메뉴
//! 바닥 줄(둘 다 `render.rs`)이고, 스레드가 끝난 뒤의 다시 그리기는 커서 깜빡임
//! 타이머가 어차피 반 주기마다 루프를 깨우므로 따로 깨울 필요가 없다.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 지금 도는 판. 워크스페이스 `Cargo.toml` 이 단일 소스다.
pub(crate) const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[cfg(windows)]
const FEED: &str = "https://2rami.github.io/kasaterm/appcast-win.xml";
#[cfg(not(windows))]
const FEED: &str = "https://2rami.github.io/kasaterm/appcast.xml";

/// 성공한 확인을 다시 하기까지. 릴리스는 하루에 몇 번 나오는 것이 아니다.
const RECHECK: Duration = Duration::from_secs(30 * 60);
/// 실패는 더 짧게 다시 본다 — 잠깐 끊긴 망 때문에 30분을 「모름」으로 보내면
/// 정작 새 판이 나온 날 그것을 못 본다.
const RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq)]
pub(crate) enum Check {
    /// 아직 한 번도 확인하지 않았다.
    Idle,
    /// 확인 중.
    Busy,
    /// 피드의 최신판이 지금 판과 같거나 더 낮다.
    Latest,
    /// 피드에 더 새 판이 있다.
    Newer(String),
    /// 확인 실패(오프라인·피드 접근 불가). 사유는 화면에 쓰지 않는다 — 사용자가
    /// 할 수 있는 일이 어차피 「나중에 다시」 하나뿐이다.
    Failed,
}

fn cell() -> &'static Mutex<(Check, Option<Instant>)> {
    static C: OnceLock<Mutex<(Check, Option<Instant>)>> = OnceLock::new();
    C.get_or_init(|| Mutex::new((Check::Idle, None)))
}

pub(crate) fn state() -> Check {
    cell().lock().map(|g| g.0.clone()).unwrap_or(Check::Idle)
}

/// 확인이 오래됐으면 백그라운드로 한 바퀴 돌린다. 그리는 자리에서 매 프레임
/// 불러도 되도록 안에서 스스로 걸러낸다.
pub(crate) fn ensure_check() {
    {
        let Ok(mut g) = cell().lock() else { return };
        if g.0 == Check::Busy {
            return;
        }
        let wait = if g.0 == Check::Failed { RETRY } else { RECHECK };
        if g.1.is_some_and(|t| t.elapsed() < wait) {
            return;
        }
        g.0 = Check::Busy;
        g.1 = Some(Instant::now());
    }
    std::thread::spawn(|| {
        let next = match fetch().and_then(|xml| crate::win_sparkle::parse_appcast_version(&xml)) {
            Some(v) if crate::win_sparkle::version_newer(&v, CURRENT) => Check::Newer(v),
            Some(_) => Check::Latest,
            None => Check::Failed,
        };
        if let Ok(mut g) = cell().lock() {
            g.0 = next;
            g.1 = Some(Instant::now());
        }
    });
}

fn fetch() -> Option<String> {
    #[cfg(windows)]
    let curl = "curl.exe";
    #[cfg(not(windows))]
    let curl = "curl";
    let out = std::process::Command::new(curl)
        .args(["-fsS", "--max-time", "8", FEED])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
