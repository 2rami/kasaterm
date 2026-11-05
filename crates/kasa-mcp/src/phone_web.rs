//! 폰 웹뷰 조종 — 에이전트가 폰 앱 안의 웹뷰(WKWebView)에 페이지를 띄우고, 화면을 찍고, 글을 읽고, JS 를 돌린다.
//!
//! 데스크톱은 폰에 먼저 닿을 길이 없다(폰이 늘 손님이다). 그래서 명령을 여기 줄 세워 두고, 앱이 켜져 있는 동안
//! 폰이 길게 기다리며 가져간다(`GET /phone/web/next`) — 결과는 `POST /phone/web/result` 로 올린다. 앱이 꺼져 있으면
//! 알림으로 열어 달라고 한다. 사람이 보는 링크(`open_url_to_phone`)는 그대로 Safari 보기로 가고, 이 길은 에이전트가
//! 다룰 수 있는 폰 안 브라우저다.
//!
//! 명령마다 받을 앱([`TARGETS`])이 있다. 카사브라우저 폰 앱(`browser`)과 카사텀 폰 앱의 실험 웹뷰(`kasaterm`)는
//! 창구부터 다르다(`/phone/browser/*`·`/phone/web/*`, RPC `phone.browser`·`phone.web`) — 이 구분을 모르는 옛
//! 데스크톱은 새 창구에 404·모르는 명령으로 답해, 한 앱의 명령이 다른 앱으로 새지 않는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 폰이 할 수 있는 일. `arg` 는 open 의 주소, eval 의 JS 원문이고 나머지는 비운다.
pub const OPS: &[&str] = &["open", "shot", "text", "eval", "url", "back", "forward", "reload"];

/// 명령을 받을 폰 앱.
pub const TARGETS: &[&str] = &["browser", "kasaterm"];

/// 폰이 돌려준 답 — 화면은 PNG 그대로, 나머지는 JSON.
pub enum Outcome {
    Json(serde_json::Value),
    Png(Vec<u8>),
}

struct Command {
    id: String,
    target: &'static str,
    op: String,
    arg: String,
    made: Instant,
}

/// 가져가지 않은 명령은 이만큼 뒤 버린다 — 늦게 열린 앱이 옛 명령을 갑자기 실행하면 안 된다.
const STALE: Duration = Duration::from_secs(120);
/// 폰의 긴 기다림 한 번(25초)에 여유를 더한 것 — 이 안에 물으러 왔으면 앱이 켜져 있다.
const LISTENING: Duration = Duration::from_secs(35);
/// 깨우는 알림 사이 — 명령 여럿이 알림 여럿이 되지 않게.
const WAKE_EVERY: Duration = Duration::from_secs(60);

static QUEUE: Mutex<VecDeque<Command>> = Mutex::new(VecDeque::new());
type Waiter = (&'static str, mpsc::Sender<Result<Outcome, String>>);
static WAITERS: LazyLock<Mutex<HashMap<String, Waiter>>> = LazyLock::new(Default::default);
static LAST_POLL: LazyLock<Mutex<HashMap<&'static str, Instant>>> = LazyLock::new(Default::default);
static LAST_WAKE: LazyLock<Mutex<HashMap<&'static str, Instant>>> = LazyLock::new(Default::default);
static ARRIVED: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 받는 앱 이름을 정해진 것으로 — 모르는 이름이면 None.
pub fn target(name: &str) -> Option<&'static str> {
    TARGETS.iter().copied().find(|t| *t == name)
}

fn polled(target: &'static str) {
    lock(&LAST_POLL).insert(target, Instant::now());
}

/// 그 폰 앱이 지금 명령을 기다리고 있나.
pub fn listening(target: &'static str) -> bool {
    lock(&LAST_POLL).get(target).is_some_and(|t| t.elapsed() < LISTENING)
}

/// 명령을 폰의 [`TARGETS`] 앱에 맡기고 답을 기다린다(블로킹 — 런타임 밖 스레드에서 부른다).
pub fn call(target_name: &str, op: &str, arg: &str, timeout: Duration) -> Result<Outcome, String> {
    let Some(target) = target(target_name) else {
        return Err(format!("target 은 {} 중 하나", TARGETS.join("|")));
    };
    if !OPS.contains(&op) {
        return Err(format!("op 는 {} 중 하나", OPS.join("|")));
    }
    if (op == "open" || op == "eval") && arg.trim().is_empty() {
        return Err(format!("{op} 에는 {} 가 필요하다", if op == "open" { "주소" } else { "JS" }));
    }
    let id = new_id();
    let (tx, rx) = mpsc::channel();
    lock(&WAITERS).insert(id.clone(), (target, tx));
    lock(&QUEUE).push_back(Command { id: id.clone(), target, op: op.to_string(), arg: arg.to_string(), made: Instant::now() });
    ARRIVED.notify_waiters();
    let awake = listening(target);
    if !awake {
        wake_phone(target);
    }
    let answer = rx.recv_timeout(timeout);
    lock(&WAITERS).remove(&id);
    lock(&QUEUE).retain(|c| c.id != id);
    match answer {
        Ok(result) => result,
        Err(_) if awake || listening(target) => Err("폰이 제때 답하지 않았다 — 페이지가 무거우면 --timeout 을 늘린다".into()),
        Err(_) => Err(format!("폰의 {} 앱이 열려 있지 않다 — 열어 달라는 알림을 보냈다. 앱을 연 뒤 다시 부른다", app_name(target))),
    }
}

fn app_name(target: &str) -> &'static str {
    if target == "browser" { "카사브라우저" } else { "카사텀" }
}

/// 앱이 꺼져 있을 때 사람에게 열어 달라는 알림. 알림 전송은 비동기라 제 스레드에서 런타임을 세운다.
/// 알림은 카사텀 폰 앱으로 간다 — 카사브라우저 앱은 아직 제 알림 길이 없다.
fn wake_phone(target: &'static str) {
    {
        let mut last = lock(&LAST_WAKE);
        if last.get(target).is_some_and(|t| t.elapsed() < WAKE_EVERY) {
            return;
        }
        last.insert(target, Instant::now());
    }
    std::thread::spawn(move || {
        let app = app_name(target);
        let alert = crate::push::Alert {
            title: format!("{app} 폰 브라우저"),
            body: format!("에이전트가 폰 브라우저를 쓰려고 해요. {app} 앱을 열어 주세요."),
            machine: None,
            pane: String::new(),
            kind: "phone-web".into(),
            collapse: Some(format!("phone-web-{target}")),
            sender: None,
            avatar_slug: None,
            url: None,
        };
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(crate::push::send(&alert));
        }
    });
}

/// [`TARGETS`] 앱이 가져갈 다음 명령 — 없으면 `wait` 동안 기다린다. 오래된 것은 버린다.
pub async fn next(target: &'static str, wait: Duration) -> Option<(String, String, String)> {
    polled(target);
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let arrived = ARRIVED.notified();
        tokio::pin!(arrived);
        // 줄을 보기 전에 기다림을 걸어 둔다 — 그 사이 들어온 명령의 알림을 놓치지 않는다.
        arrived.as_mut().enable();
        {
            let mut queue = lock(&QUEUE);
            queue.retain(|c| c.made.elapsed() < STALE);
            if let Some(at) = queue.iter().position(|c| c.target == target) {
                let c = queue.remove(at).expect("position is in the queue");
                return Some((c.id, c.op, c.arg));
            }
        }
        if tokio::time::timeout_at(deadline, arrived).await.is_err() {
            polled(target);
            return None;
        }
    }
}

/// [`TARGETS`] 앱이 올린 결과를 기다리던 쪽에 넘긴다. 기다리는 쪽이 이미 포기했거나 다른 앱에 맡긴 명령이면 false.
pub fn deliver(target: &'static str, id: &str, outcome: Result<Outcome, String>) -> bool {
    let mut waiters = lock(&WAITERS);
    if waiters.get(id).is_none_or(|(t, _)| *t != target) {
        return false;
    }
    let (_, tx) = waiters.remove(id).expect("checked above");
    drop(waiters);
    polled(target);
    tx.send(outcome).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_command_reaches_its_app_and_the_answer_comes_back() {
        polled("kasaterm");
        let caller = std::thread::spawn(|| call("kasaterm", "text", "", Duration::from_secs(5)));
        let (id, op, arg) = next("kasaterm", Duration::from_secs(5)).await.expect("the phone got no command");
        assert_eq!((op.as_str(), arg.as_str()), ("text", ""));
        assert!(!deliver("browser", &id, Ok(Outcome::Json(serde_json::json!({})))), "another app answered");
        assert!(deliver("kasaterm", &id, Ok(Outcome::Json(serde_json::json!({"text": "본문"})))));
        match caller.join().unwrap() {
            Ok(Outcome::Json(v)) => assert_eq!(v["text"], "본문"),
            _ => panic!("the caller did not get the answer"),
        }
        assert!(!deliver("kasaterm", &id, Ok(Outcome::Json(serde_json::json!({})))), "an answer was taken twice");
        assert!(call("kasaterm", "jump", "", Duration::from_millis(10)).is_err());
        assert!(call("kasaterm", "open", " ", Duration::from_millis(10)).is_err());
        assert!(call("tv", "url", "", Duration::from_millis(10)).is_err());

        // 같은 줄이어도 받는 앱이 다르면 가져가지 않는다(한 시험 안 — 줄이 전역이라 시험끼리 나눠 갖지 않게).
        polled("browser");
        let for_browser = std::thread::spawn(|| call("browser", "eval", "1+1", Duration::from_secs(5)));
        let (id, op, arg) = next("browser", Duration::from_secs(5)).await.expect("the browser got no command");
        assert_eq!((op.as_str(), arg.as_str()), ("eval", "1+1"));
        assert!(next("kasaterm", Duration::from_millis(50)).await.is_none(), "the other app took a browser command");
        assert!(deliver("browser", &id, Ok(Outcome::Json(serde_json::json!({"value": 2})))));
        assert!(matches!(for_browser.join().unwrap(), Ok(Outcome::Json(v)) if v["value"] == 2));
    }
}
