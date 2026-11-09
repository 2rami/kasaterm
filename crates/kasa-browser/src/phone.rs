//! 폰 웹뷰 조종 — 에이전트가 카사브라우저 폰 앱의 웹뷰(WKWebView)에 페이지를 띄우고, 화면을 찍고, 글을 읽고,
//! JS 를 돌린다.
//!
//! 데스크톱은 폰에 먼저 닿을 길이 없다(폰이 늘 손님이다). 그래서 명령을 여기 줄 세워 두고, 앱이 켜져 있는 동안
//! 폰이 길게 기다리며 가져간다(`GET /phone/browser/next`) — 결과는 `POST /phone/browser/result` 로 올린다. 앱이
//! 꺼져 있으면 알림으로 열어 달라고 한다. 사람이 보는 링크(`open_url_to_phone`)는 그대로 카사텀 폰 앱의 Safari
//! 보기로 간다.
//!
//! 창구·RPC 이름(`phone.browser`)이 카사브라우저 앱 전용이다. 카사텀 폰 앱이 실험으로 쓰던 `/phone/web/*` 는
//! 없앴다 — 그 판의 앱이 물으면 404 로 멈추고, 웹뷰 명령은 카사브라우저 앱만 받는다.

use std::collections::{HashMap, VecDeque};
use std::sync::{mpsc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 폰이 할 수 있는 일. `arg` 는 open 의 주소, eval 의 JS 원문이고 나머지는 비운다.
pub const OPS: &[&str] = &["open", "shot", "text", "eval", "url", "back", "forward", "reload"];

/// 폰이 돌려준 답 — 화면은 PNG 그대로, 나머지는 JSON.
pub enum Outcome {
    Json(serde_json::Value),
    Png(Vec<u8>),
}

struct Command {
    id: String,
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
static WAITERS: LazyLock<Mutex<HashMap<String, mpsc::Sender<Result<Outcome, String>>>>> =
    LazyLock::new(Default::default);
static LAST_POLL: Mutex<Option<Instant>> = Mutex::new(None);
static LAST_WAKE: Mutex<Option<Instant>> = Mutex::new(None);
static ARRIVED: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 폰 앱이 지금 명령을 기다리고 있나.
pub fn listening() -> bool {
    lock(&LAST_POLL).is_some_and(|t| t.elapsed() < LISTENING)
}

/// 명령을 폰에 맡기고 답을 기다린다(블로킹 — 런타임 밖 스레드에서 부른다).
pub fn call(op: &str, arg: &str, timeout: Duration) -> Result<Outcome, String> {
    if !OPS.contains(&op) {
        return Err(format!("op 는 {} 중 하나", OPS.join("|")));
    }
    if (op == "open" || op == "eval") && arg.trim().is_empty() {
        return Err(format!("{op} 에는 {} 가 필요하다", if op == "open" { "주소" } else { "JS" }));
    }
    let id = new_id();
    let (tx, rx) = mpsc::channel();
    lock(&WAITERS).insert(id.clone(), tx);
    lock(&QUEUE).push_back(Command { id: id.clone(), op: op.to_string(), arg: arg.to_string(), made: Instant::now() });
    ARRIVED.notify_waiters();
    let awake = listening();
    if !awake {
        wake_phone();
    }
    let answer = rx.recv_timeout(timeout);
    lock(&WAITERS).remove(&id);
    lock(&QUEUE).retain(|c| c.id != id);
    match answer {
        Ok(result) => result,
        Err(_) if awake || listening() => Err("폰이 제때 답하지 않았다 — 페이지가 무거우면 --timeout 을 늘린다".into()),
        Err(_) => Err("폰의 카사브라우저 앱이 열려 있지 않다 — 열어 달라는 알림을 보냈다. 앱을 연 뒤 다시 부른다".into()),
    }
}

/// 앱이 꺼져 있을 때 사람에게 열어 달라는 알림. 알림 전송은 비동기라 제 스레드에서 런타임을 세운다.
/// 알림은 카사텀 폰 앱으로 간다 — 카사브라우저 앱은 아직 제 알림 길이 없다.
fn wake_phone() {
    {
        let mut last = lock(&LAST_WAKE);
        if last.is_some_and(|t| t.elapsed() < WAKE_EVERY) {
            return;
        }
        *last = Some(Instant::now());
    }
    std::thread::spawn(|| {
        let alert = kasa_device::push::Alert {
            title: "카사브라우저".into(),
            body: "에이전트가 폰 브라우저를 쓰려고 해요. 카사브라우저 앱을 열어 주세요.".into(),
            machine: None,
            pane: String::new(),
            kind: "phone-web".into(),
            collapse: Some("phone-web".into()),
            sender: None,
            avatar_slug: None,
            url: None,
        };
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            rt.block_on(kasa_device::push::send(&alert));
        }
    });
}

/// 폰이 가져갈 다음 명령 — 없으면 `wait` 동안 기다린다. 오래된 것은 버린다.
pub async fn next(wait: Duration) -> Option<(String, String, String)> {
    *lock(&LAST_POLL) = Some(Instant::now());
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let arrived = ARRIVED.notified();
        tokio::pin!(arrived);
        // 줄을 보기 전에 기다림을 걸어 둔다 — 그 사이 들어온 명령의 알림을 놓치지 않는다.
        arrived.as_mut().enable();
        {
            let mut queue = lock(&QUEUE);
            queue.retain(|c| c.made.elapsed() < STALE);
            if let Some(c) = queue.pop_front() {
                return Some((c.id, c.op, c.arg));
            }
        }
        if tokio::time::timeout_at(deadline, arrived).await.is_err() {
            *lock(&LAST_POLL) = Some(Instant::now());
            return None;
        }
    }
}

/// 폰이 올린 결과를 기다리던 쪽에 넘긴다. 기다리는 쪽이 이미 포기했으면 false.
pub fn deliver(id: &str, outcome: Result<Outcome, String>) -> bool {
    *lock(&LAST_POLL) = Some(Instant::now());
    match lock(&WAITERS).remove(id) {
        Some(tx) => tx.send(outcome).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn a_command_reaches_the_phone_and_its_answer_comes_back() {
        *lock(&LAST_POLL) = Some(Instant::now());
        let caller = std::thread::spawn(|| call("text", "", Duration::from_secs(5)));
        let (id, op, arg) = next(Duration::from_secs(5)).await.expect("the phone got no command");
        assert_eq!((op.as_str(), arg.as_str()), ("text", ""));
        assert!(deliver(&id, Ok(Outcome::Json(serde_json::json!({"text": "본문"})))));
        match caller.join().unwrap() {
            Ok(Outcome::Json(v)) => assert_eq!(v["text"], "본문"),
            _ => panic!("the caller did not get the answer"),
        }
        assert!(!deliver(&id, Ok(Outcome::Json(serde_json::json!({})))), "an answer was taken twice");
        assert!(call("jump", "", Duration::from_millis(10)).is_err());
        assert!(call("open", " ", Duration::from_millis(10)).is_err());
    }
}
