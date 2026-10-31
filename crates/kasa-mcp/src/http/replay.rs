//! 폰이 다시 보낸 요청을 한 번만 — `x-kasa-request-id` 를 단 요청은 처음 온 것만 처리하고, 같은 표식이 다시 오면
//! 처음 것의 답을 그대로 준다.
//!
//! 폰은 답을 못 받으면 같은 표식으로 관문에 다시 보낸다(`mobile/lib/server.dart` `_mutate`). 셀룰러에서 카사넷 직통이
//! 죽어도 앱 안 입구는 한동안 「직통」이라 답하며 요청을 삼켜, 첫 요청이 여기 닿았는지 폰은 알 수 없다 — 표식이
//! 없으면 다시 보낸 키 입력·사진·칸 닫기가 두 번 일어난다.
//!
//! - 표식은 몸통을 다 받은 뒤에 잡는다. 몸통이 길에서 멈춘 첫 요청이 표식을 쥔 채 다시 보내기를 막지 않게.
//! - 처리는 따로 띄운다. 폰이 기다리다 끊으면 axum 은 처리기를 버리는데, 반쯤 한 일을 다시 보내기가 또 하지 않게
//!   처음 것이 끝까지 가서 답을 남긴다.
//! - 같은 표식이 처리 중이면 끝날 때까지 기다렸다 같은 답을 준다.
//! - 답은 10분·256개까지 메모리에만 둔다. 폰의 다시 보내기는 수십 초 안이다.

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub(crate) const HEADER: &str = "x-kasa-request-id";
const KEEP: Duration = Duration::from_secs(600);
const MAX_KEPT: usize = 256;
/// 사진(32MB)을 받는 창구가 있다 — 그보다 조금 넉넉히.
const MAX_BODY: usize = 40 << 20;
/// 이 창구들의 답은 짧은 JSON 이다.
const MAX_ANSWER: usize = 1 << 20;
const WAIT_SAME: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Answer {
    async fn of(response: Response) -> Self {
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, MAX_ANSWER).await {
            Ok(body) => Self { status: parts.status, headers: parts.headers, body },
            Err(_) => Self::failed("answer_unreadable"),
        }
    }

    fn failed(code: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let body = serde_json::json!({"ok": false, "error": code}).to_string();
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, headers, body: body.into() }
    }

    fn respond(self, replayed: bool) -> Response {
        let mut response = (self.status, self.body).into_response();
        for (name, value) in &self.headers {
            if name != axum::http::header::CONTENT_LENGTH {
                response.headers_mut().insert(name, value.clone());
            }
        }
        if replayed {
            response.headers_mut().insert("x-kasa-replayed", HeaderValue::from_static("1"));
        }
        response
    }
}

enum Slot {
    Running(watch::Receiver<Option<Answer>>),
    Done(Answer, Instant),
}

static SLOTS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(Default::default);

enum Claim {
    Mine(watch::Sender<Option<Answer>>),
    Running(watch::Receiver<Option<Answer>>),
    Done(Answer),
}

fn request_id(req: &Request) -> Option<String> {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return None;
    }
    let id = req.headers().get(HEADER)?.to_str().ok()?;
    let ok = (16..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    ok.then(|| id.to_string())
}

fn claim(key: &str, now: Instant) -> Claim {
    let mut slots = SLOTS.lock().unwrap();
    slots.retain(|_, slot| !matches!(slot, Slot::Done(_, at) if now.duration_since(*at) >= KEEP));
    let done = slots.iter().filter(|(_, slot)| matches!(slot, Slot::Done(..))).count();
    if done >= MAX_KEPT {
        let mut oldest: Vec<(Instant, String)> = slots
            .iter()
            .filter_map(|(k, slot)| match slot {
                Slot::Done(_, at) => Some((*at, k.clone())),
                Slot::Running(_) => None,
            })
            .collect();
        oldest.sort();
        for (_, k) in oldest.into_iter().take(done + 1 - MAX_KEPT) {
            slots.remove(&k);
        }
    }
    match slots.get(key) {
        Some(Slot::Done(answer, _)) => Claim::Done(answer.clone()),
        Some(Slot::Running(rx)) => Claim::Running(rx.clone()),
        None => {
            let (tx, rx) = watch::channel(None);
            slots.insert(key.to_string(), Slot::Running(rx));
            Claim::Mine(tx)
        }
    }
}

/// 처리 하나의 수명. 답을 남기지 못하고 끝나면(처리기가 죽음) 표식을 풀어 다시 보내기가 들어가게 한다.
struct Running {
    key: String,
    tx: watch::Sender<Option<Answer>>,
    finished: bool,
}

impl Running {
    fn finish(mut self, answer: &Answer) {
        SLOTS.lock().unwrap().insert(self.key.clone(), Slot::Done(answer.clone(), Instant::now()));
        let _ = self.tx.send(Some(answer.clone()));
        self.finished = true;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if !self.finished {
            SLOTS.lock().unwrap().remove(&self.key);
        }
    }
}

pub(crate) async fn replay_mw(req: Request, next: Next) -> Response {
    let Some(id) = request_id(&req) else { return next.run(req).await };
    let key = format!("{}\n{id}", req.uri().path());
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return Answer::failed("body_unreadable").respond(false);
    };
    let mut req = Some(Request::from_parts(parts, Body::from(bytes)));
    // 기다리던 처리가 답 없이 죽었으면 한 번 더 표식을 잡아 본다.
    for _ in 0..2 {
        match claim(&key, Instant::now()) {
            Claim::Done(answer) => return answer.respond(true),
            Claim::Running(mut rx) => {
                match tokio::time::timeout(WAIT_SAME, rx.wait_for(Option::is_some)).await {
                    Ok(Ok(answer)) => return answer.clone().expect("waited for an answer").respond(true),
                    Ok(Err(_)) => continue,
                    Err(_) => {
                        let body = serde_json::json!({"ok": false, "error": "in_progress"});
                        return (StatusCode::CONFLICT, axum::Json(body)).into_response();
                    }
                }
            }
            Claim::Mine(tx) => {
                let running = Running { key: key.clone(), tx, finished: false };
                let req = req.take().expect("claimed once");
                let task = tokio::spawn(async move {
                    let answer = Answer::of(next.run(req).await).await;
                    running.finish(&answer);
                    answer
                });
                return match task.await {
                    Ok(answer) => answer.respond(false),
                    Err(_) => Answer::failed("handler_failed").respond(false),
                };
            }
        }
    }
    Answer::failed("handler_failed").respond(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tower::ServiceExt as _;

    fn app(count: Arc<AtomicUsize>, delay: Duration) -> axum::Router {
        axum::Router::new()
            .route(
                "/act",
                axum::routing::post(move |body: Bytes| {
                    let count = count.clone();
                    async move {
                        tokio::time::sleep(delay).await;
                        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
                        axum::Json(serde_json::json!({"ok": true, "n": n, "len": body.len()}))
                    }
                })
                .get(|| async { "read" }),
            )
            .layer(axum::middleware::from_fn(replay_mw))
    }

    fn post(id: Option<&str>) -> Request {
        let mut builder = Request::builder().method("POST").uri("/act");
        if let Some(id) = id {
            builder = builder.header(HEADER, id);
        }
        builder.body(Body::from("payload")).unwrap()
    }

    fn fresh_id() -> String {
        format!("rq{}", uuid::Uuid::new_v4().simple())
    }

    async fn read(response: Response) -> (StatusCode, bool, serde_json::Value) {
        let replayed = response.headers().contains_key("x-kasa-replayed");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), MAX_ANSWER).await.unwrap();
        (status, replayed, serde_json::from_slice(&body).unwrap_or_default())
    }

    #[tokio::test]
    async fn the_same_id_runs_once_and_answers_the_same() {
        let count = Arc::new(AtomicUsize::new(0));
        let app = app(count.clone(), Duration::ZERO);
        let id = fresh_id();
        let (status, replayed, first) = read(app.clone().oneshot(post(Some(&id))).await.unwrap()).await;
        assert_eq!((status, replayed), (StatusCode::OK, false));
        let (status, replayed, again) = read(app.clone().oneshot(post(Some(&id))).await.unwrap()).await;
        assert_eq!((status, replayed), (StatusCode::OK, true));
        assert_eq!(first, again);
        assert_eq!(first["len"], 7);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        app.clone().oneshot(post(Some(&fresh_id()))).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn requests_without_a_valid_id_and_reads_pass_through() {
        let count = Arc::new(AtomicUsize::new(0));
        let app = app(count.clone(), Duration::ZERO);
        for id in [None, Some("short"), Some("has space in the identifier!")] {
            app.clone().oneshot(post(id)).await.unwrap();
            app.clone().oneshot(post(id)).await.unwrap();
        }
        assert_eq!(count.load(Ordering::SeqCst), 6);
        let id = fresh_id();
        for _ in 0..2 {
            let get = Request::builder().uri("/act").header(HEADER, &id).body(Body::empty()).unwrap();
            assert_eq!(app.clone().oneshot(get).await.unwrap().status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn a_resend_while_the_first_is_running_waits_for_its_answer() {
        let count = Arc::new(AtomicUsize::new(0));
        let app = app(count.clone(), Duration::from_millis(200));
        let id = fresh_id();
        let first = tokio::spawn(app.clone().oneshot(post(Some(&id))));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (_, replayed, again) = read(app.clone().oneshot(post(Some(&id))).await.unwrap()).await;
        let (_, _, original) = read(first.await.unwrap().unwrap()).await;
        assert!(replayed);
        assert_eq!(again, original);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// 폰이 기다리다 끊은 첫 요청도 끝까지 가서 답을 남긴다 — 다시 보내기가 같은 일을 또 하지 않는다.
    #[tokio::test]
    async fn an_abandoned_first_request_still_finishes_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let app = app(count.clone(), Duration::from_millis(150));
        let id = fresh_id();
        let first = tokio::spawn(app.clone().oneshot(post(Some(&id))));
        tokio::time::sleep(Duration::from_millis(30)).await;
        first.abort();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let (_, replayed, again) = read(app.clone().oneshot(post(Some(&id))).await.unwrap()).await;
        assert!(replayed);
        assert_eq!(again["n"], 1);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
