//! 본진(홈 기계)의 계정 칸을 이 기계 설정창에서 다루는 통로.
//!
//! 계정 등록·자동 전환은 **학생이 실제로 도는 기계**에서 일어나야 한다. 본진
//! 디스패치를 켠 뒤로 순정 `claude` 는 본진에서 태어나는데, 계정 목록·사용량
//! 조회·전환은 기계마다 따로인 로컬 상태라 그대로 갈라져 있었다 — 2026-09-05
//! 실측으로 작업대는 슬롯 4개·자동전환 켜짐·80%, 본진은 슬롯 0개·꺼짐·90% 였다.
//! 그래서 설정창에서 계정을 아무리 등록해도 실제로 한도가 차는 기계에는 아무것도
//! 없었고, 85% 자동 전환이 영영 일어나지 않았다.
//!
//! 여기서 오가는 것은 **목록·스위치·기준값과 OAuth 코드 한 줄**뿐이다. 자격증명
//! 자체는 옮기지 않는다(`kasa_mcp::remote` 의 같은 절 참고).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use kasa_mcp::remote::RemoteAccounts;

/// 폴링 간격. 계정 칸은 사람이 보고 있을 때만 갱신하면 되고, 그 화면에서 값이
/// 초 단위로 변하는 것은 사용량뿐이다.
const POLL_EVERY: Duration = Duration::from_secs(5);

struct Cache {
    label: String,
    base: String,
    value: Option<RemoteAccounts>,
    /// 마지막으로 **시도한** 시각. 실패해도 갱신한다 — 안 그러면 죽은 기계에
    /// 매 프레임 붙는다.
    tried: Option<Instant>,
    inflight: bool,
    /// 마지막 조회 실패 이유. 화면이 「못 읽었다」를 말할 수 있어야 한다.
    error: Option<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| {
        Mutex::new(Cache {
            label: String::new(),
            base: String::new(),
            value: None,
            tried: None,
            inflight: false,
            error: None,
        })
    })
}

/// 원격 동작의 결과 말풍선 대기줄. 동작이 백그라운드 스레드에서 끝나므로
/// `App` 에 바로 못 쓴다 — 렌더 쪽에서 비워 토스트로 띄운다.
fn toasts() -> &'static Mutex<Vec<String>> {
    static T: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn drain_toasts() -> Vec<String> {
    toasts()
        .lock()
        .map(|mut t| std::mem::take(&mut *t))
        .unwrap_or_default()
}

fn push_toast(msg: String) {
    if let Ok(mut t) = toasts().lock() {
        // 같은 말이 연달아 쌓이지 않게 — 폴링이 겹치면 같은 실패가 반복된다.
        if t.last().map(String::as_str) != Some(msg.as_str()) {
            t.push(msg);
        }
    }
}

/// 계정 칸을 본진 것으로 보여줄 자리인가 — `(기계 이름, base)`.
///
/// **살아 있을 때만** 돌려준다. 꺼진 기계의 계정 칸을 그리면 사용자는 목록이
/// 비어 보이는 이유를 알 길이 없고, 그 상태로 「계정 추가」를 누르면 조용히
/// 아무 일도 안 난다.
pub(crate) fn home_target() -> Option<(String, String)> {
    let m = kasa_mcp::machines::home_machine()?;
    let online = kasa_mcp::machines::snapshot()
        .iter()
        .find(|v| v.get("label").and_then(|l| l.as_str()) == Some(m.label.as_str()))
        .and_then(|v| v.get("online").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    online.then_some((m.label, m.base))
}

/// 설정창이 매 프레임 부른다. 실제 조회는 간격을 두고 별도 스레드에서 한다 —
/// 렌더 스레드에서 HTTP 를 기다리면 창이 통째로 멈춘다.
pub(crate) fn poll() {
    let Some((label, base)) = home_target() else {
        if let Ok(mut c) = cache().lock() {
            c.value = None;
            c.error = None;
        }
        return;
    };
    let start = {
        let Ok(mut c) = cache().lock() else { return };
        // 기계가 바뀌면 옛 기계의 목록을 새 기계 것으로 보여주지 않는다.
        if c.base != base {
            c.base.clone_from(&base);
            c.label.clone_from(&label);
            c.value = None;
            c.error = None;
            c.tried = None;
        }
        if c.inflight || c.tried.is_some_and(|t| t.elapsed() < POLL_EVERY) {
            false
        } else {
            c.inflight = true;
            c.tried = Some(Instant::now());
            true
        }
    };
    if !start {
        return;
    }
    std::thread::spawn(move || {
        let got = kasa_mcp::remote::fetch_accounts(&base, None);
        if let Ok(mut c) = cache().lock() {
            // 늦게 온 응답이 그 사이 바뀐 기계의 칸을 덮으면 안 된다.
            if c.base == base {
                match got {
                    Ok(v) => {
                        c.value = Some(v);
                        c.error = None;
                    }
                    // 마지막으로 알던 목록은 지우지 않는다 — 한 번 실패했다고
                    // 칸이 빈 것처럼 보이면 사용자는 계정이 날아갔다고 읽는다.
                    Err(e) => c.error = Some(e.to_string()),
                }
            }
            c.inflight = false;
        }
    });
}

/// 지금 알고 있는 본진 계정 칸 — `(기계 이름, 값, 실패 이유)`.
pub(crate) fn snapshot() -> Option<(String, Option<RemoteAccounts>, Option<String>)> {
    let c = cache().lock().ok()?;
    if c.base.is_empty() {
        return None;
    }
    Some((c.label.clone(), c.value.clone(), c.error.clone()))
}

/// 본진 설정창의 버튼 하나를 누른다. 결과는 토스트로 돌아온다.
///
/// 누른 직후 캐시를 만료시켜, 다음 프레임이 곧바로 새 목록을 읽게 한다 —
/// 5초를 기다리면 사용자는 눌린 줄 모르고 한 번 더 누른다.
pub(crate) fn act(action: &'static str, id: Option<String>, label: Option<String>) {
    let Some((_, base)) = home_target() else {
        push_toast("본진이 지금 안 보여요".to_string());
        return;
    };
    std::thread::spawn(move || {
        let r = kasa_mcp::remote::settings_action(
            &base,
            action,
            id.as_deref(),
            label.as_deref(),
            None,
        );
        if let Err(e) = r {
            push_toast(format!("본진에 못 전했어요: {e}"));
        }
        if let Ok(mut c) = cache().lock() {
            c.tried = None;
        }
    });
}
