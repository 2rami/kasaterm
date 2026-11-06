use super::*;

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use axum::extract::State;
use axum::http::{header, Uri};
use axum::routing::any;
use axum::Router;

#[derive(Clone)]
struct Attempt {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
}

struct Plan {
    status: StatusCode,
    content_type: &'static str,
    body: &'static str,
}

impl Plan {
    fn json(status: StatusCode, body: &'static str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body,
        }
    }

    fn sse(body: &'static str) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream",
            body,
        }
    }

    fn response(self) -> Response {
        Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, self.content_type)
            .header("x-upstream-response", "kept")
            .header(header::CONNECTION, "close")
            .body(Body::from(self.body))
            .unwrap()
    }
}

type Responder = dyn Fn(usize, &Attempt) -> Plan + Send + Sync;

#[derive(Clone)]
struct FakeUpstream {
    attempts: Arc<Mutex<Vec<Attempt>>>,
    responder: Arc<Responder>,
}

async fn fake_upstream(
    State(state): State<FakeUpstream>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let attempt = Attempt {
        method,
        path: uri.path().to_string(),
        headers,
        body,
    };
    let index = {
        let mut attempts = state.attempts.lock().unwrap();
        let index = attempts.len();
        attempts.push(attempt.clone());
        index
    };
    (state.responder)(index, &attempt).response()
}

async fn spawn_upstream(
    responder: impl Fn(usize, &Attempt) -> Plan + Send + Sync + 'static,
) -> (
    String,
    Arc<Mutex<Vec<Attempt>>>,
    tokio::task::JoinHandle<()>,
) {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let state = FakeUpstream {
        attempts: attempts.clone(),
        responder: Arc::new(responder),
    };
    let app = Router::new().fallback(any(fake_upstream)).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), attempts, server)
}

#[derive(Default)]
struct RecoveryStats {
    issued: AtomicUsize,
    recovered: AtomicUsize,
}

type RecoveryFactory = dyn Fn() -> BoxFuture<'static, Option<HeaderValue>> + Send + Sync + 'static;

struct FixtureRecovery {
    stats: Arc<RecoveryStats>,
    factory: Arc<RecoveryFactory>,
}

impl RecoveryAdapter for FixtureRecovery {
    fn issue(&self, headers: &HeaderMap) -> Option<BoxFuture<'static, Option<HeaderValue>>> {
        self.stats.issued.fetch_add(1, Ordering::SeqCst);
        let authorization = headers
            .get_all(header::AUTHORIZATION)
            .iter()
            .collect::<Vec<_>>();
        if headers.contains_key("x-api-key")
            || authorization.len() != 1
            || authorization[0].as_bytes() != b"Bearer old-managed"
        {
            return None;
        }
        let recovery = (self.factory)();
        let stats = self.stats.clone();
        Some(Box::pin(async move {
            stats.recovered.fetch_add(1, Ordering::SeqCst);
            recovery.await
        }))
    }
}

fn fixture(
    factory: impl Fn() -> BoxFuture<'static, Option<HeaderValue>> + Send + Sync + 'static,
) -> (Arc<FixtureRecovery>, Arc<RecoveryStats>) {
    let stats = Arc::new(RecoveryStats::default());
    (
        Arc::new(FixtureRecovery {
            stats: stats.clone(),
            factory: Arc::new(factory),
        }),
        stats,
    )
}

fn recovered_authorization() -> HeaderValue {
    let mut value = HeaderValue::from_static("Bearer new-managed");
    value.set_sensitive(true);
    value
}

fn immediate_fixture() -> (Arc<FixtureRecovery>, Arc<RecoveryStats>) {
    fixture(|| Box::pin(async { Some(recovered_authorization()) }))
}

fn request_headers(authorization: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static(authorization),
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    headers.append("x-preserve", HeaderValue::from_static("first"));
    headers.append("x-preserve", HeaderValue::from_static("second"));
    headers
}

async fn invoke(
    upstream: &str,
    recovery: &dyn RecoveryAdapter,
    timeout: Duration,
    method: Method,
    rest: &str,
    headers: HeaderMap,
    body: Bytes,
    direct_loopback: bool,
    store: ConvStore,
) -> Response {
    proxy_handler_inner(
        store,
        reqwest::Client::new(),
        "12".into(),
        rest.into(),
        method,
        headers,
        body,
        direct_loopback,
        recovery,
        ProxyConfig {
            upstream,
            recovery_timeout: timeout,
        },
    )
    .await
}

async fn response_bytes(response: Response) -> Bytes {
    axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
}

fn auth(attempt: &Attempt) -> &[u8] {
    attempt
        .headers
        .get(header::AUTHORIZATION)
        .map(HeaderValue::as_bytes)
        .unwrap_or_default()
}

#[tokio::test]
async fn old_401_retries_once_with_only_authorization_changed() {
    let (recovery, stats) = immediate_fixture();
    let issued = stats.clone();
    let (upstream, attempts, server) = spawn_upstream(move |index, attempt| {
        if index == 0 {
            assert_eq!(issued.issued.load(Ordering::SeqCst), 1);
            assert_eq!(auth(attempt), b"Bearer old-managed");
            Plan::json(StatusCode::UNAUTHORIZED, r#"{"error":{"message":"old"}}"#)
        } else {
            assert_eq!(auth(attempt), b"Bearer new-managed");
            Plan::json(StatusCode::OK, r#"{"ok":true}"#)
        }
    })
    .await;
    let body = Bytes::from_static(br#"{"same":"body"}"#);
    let response = invoke(
        &upstream,
        recovery.as_ref(),
        Duration::from_secs(1),
        Method::POST,
        "v1/messages",
        request_headers("Bearer old-managed"),
        body.clone(),
        true,
        Default::default(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-upstream-response"], "kept");
    assert_eq!(response_bytes(response).await, r#"{"ok":true}"#);
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].method, Method::POST);
    assert_eq!(attempts[0].path, "/v1/messages");
    assert_eq!(attempts[0].body, body);
    assert_eq!(attempts[1].body, body);
    let mut first_headers = attempts[0].headers.clone();
    let mut second_headers = attempts[1].headers.clone();
    first_headers.remove(header::AUTHORIZATION);
    second_headers.remove(header::AUTHORIZATION);
    assert_eq!(first_headers, second_headers);
    assert_eq!(stats.issued.load(Ordering::SeqCst), 1);
    assert_eq!(stats.recovered.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn recovered_second_401_is_not_retried_again() {
    let (recovery, stats) = immediate_fixture();
    let (upstream, attempts, server) = spawn_upstream(|_, _| {
        Plan::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"still unauthorized"}}"#,
        )
    })
    .await;
    let response = invoke(
        &upstream,
        recovery.as_ref(),
        Duration::from_secs(1),
        Method::POST,
        "v1/messages/count_tokens",
        request_headers("Bearer old-managed"),
        Bytes::from_static(b"{}"),
        true,
        Default::default(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response_bytes(response).await;
    assert_eq!(body, r#"{"error":{"message":"still unauthorized"}}"#);
    assert!(!String::from_utf8_lossy(&body).contains("gateway OK"));
    assert_eq!(attempts.lock().unwrap().len(), 2);
    assert_eq!(stats.recovered.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn policy_rejects_wrong_auth_origin_method_path_and_status() {
    let (upstream, attempts, server) = spawn_upstream(|_, attempt| {
        match attempt
            .headers
            .get("x-status")
            .and_then(|v| v.to_str().ok())
        {
            Some("403") => Plan::json(StatusCode::FORBIDDEN, "forbidden"),
            Some("429") => Plan::json(StatusCode::TOO_MANY_REQUESTS, "busy"),
            Some("500") => Plan::json(StatusCode::INTERNAL_SERVER_ERROR, "broken"),
            Some("401-sse") => Plan {
                status: StatusCode::UNAUTHORIZED,
                content_type: "text/event-stream",
                body: "data: {\"type\":\"error\"}\n\n",
            },
            Some("200-sse") => Plan::sse("data: {\"type\":\"error\"}\n\n"),
            _ => Plan::json(StatusCode::UNAUTHORIZED, "old"),
        }
    })
    .await;

    struct Case {
        method: Method,
        rest: &'static str,
        headers: HeaderMap,
        direct: bool,
        issued: usize,
        recovered: usize,
    }

    let mut api_key = request_headers("Bearer old-managed");
    api_key.insert("x-api-key", HeaderValue::from_static("api-key"));
    let mut multiple = request_headers("Bearer old-managed");
    multiple.append(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer another"),
    );
    let status = |value: &'static str| {
        let mut headers = request_headers("Bearer old-managed");
        headers.insert("x-status", HeaderValue::from_static(value));
        headers
    };
    let cases = vec![
        Case {
            method: Method::GET,
            rest: "v1/messages",
            headers: request_headers("Bearer old-managed"),
            direct: true,
            issued: 0,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/complete",
            headers: request_headers("Bearer old-managed"),
            direct: true,
            issued: 0,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: api_key,
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: request_headers("Bearer unknown"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: request_headers("Bearer sk-ant-api03-not-oauth"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: multiple,
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: request_headers("Bearer old-managed"),
            direct: false,
            issued: 0,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: status("403"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: status("429"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: status("500"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: status("401-sse"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
        Case {
            method: Method::POST,
            rest: "v1/messages",
            headers: status("200-sse"),
            direct: true,
            issued: 1,
            recovered: 0,
        },
    ];

    for case in cases {
        let (recovery, stats) = immediate_fixture();
        let response = invoke(
            &upstream,
            recovery.as_ref(),
            Duration::from_millis(100),
            case.method,
            case.rest,
            case.headers,
            Bytes::from_static(b"{}"),
            case.direct,
            Default::default(),
        )
        .await;
        let _ = response_bytes(response).await;
        assert_eq!(stats.issued.load(Ordering::SeqCst), case.issued);
        assert_eq!(stats.recovered.load(Ordering::SeqCst), case.recovered);
    }
    assert_eq!(attempts.lock().unwrap().len(), 12);
    server.abort();
}

#[tokio::test]
async fn retry_sse_keeps_capture_streaming() {
    const SSE: &str = concat!(
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"recovered\"}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n"
    );
    let (recovery, _) = immediate_fixture();
    let (upstream, attempts, server) = spawn_upstream(|index, _| {
        if index == 0 {
            Plan::json(StatusCode::UNAUTHORIZED, "old")
        } else {
            Plan::sse(SSE)
        }
    })
    .await;
    let store: ConvStore = Default::default();
    let body = Bytes::from_static(
        br#"{"model":"claude-test","system":"You are Claude Code","tools":[{"name":"Read"}],"messages":[{"role":"user","content":"hello"}]}"#,
    );
    let response = invoke(
        &upstream,
        recovery.as_ref(),
        Duration::from_secs(1),
        Method::POST,
        "v1/messages",
        request_headers("Bearer old-managed"),
        body,
        true,
        store.clone(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .contains("event-stream"));
    assert_eq!(response_bytes(response).await, SSE);
    let conversation = conversation_json(&store, "12");
    assert_eq!(conversation["turns"][0]["text"], "hello");
    assert_eq!(conversation["turns"][1]["text"], "recovered");
    assert_eq!(conversation["streaming"], "");
    assert_eq!(attempts.lock().unwrap().len(), 2);
    server.abort();
}

struct DropMark(Arc<AtomicBool>);

impl Drop for DropMark {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn recovery_timeout_drops_attempt_and_returns_annotated_first_401() {
    let dropped = Arc::new(AtomicBool::new(false));
    let dropped_in_future = dropped.clone();
    let (recovery, stats) = fixture(move || {
        let dropped = dropped_in_future.clone();
        Box::pin(async move {
            let _drop = DropMark(dropped);
            futures_util::future::pending::<Option<HeaderValue>>().await
        })
    });
    let (upstream, attempts, server) = spawn_upstream(|_, _| {
        Plan::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"original"}}"#,
        )
    })
    .await;
    let response = invoke(
        &upstream,
        recovery.as_ref(),
        Duration::from_millis(25),
        Method::POST,
        "v1/messages",
        request_headers("Bearer old-managed"),
        Bytes::from_static(b"{}"),
        true,
        Default::default(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = String::from_utf8_lossy(&response_bytes(response).await).into_owned();
    assert!(body.contains("original"));
    assert!(body.contains("gateway OK"));
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(attempts.lock().unwrap().len(), 1);
    assert_eq!(stats.recovered.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn recovery_none_returns_annotated_first_401() {
    let (recovery, stats) = fixture(|| Box::pin(async { None }));
    let (upstream, attempts, server) = spawn_upstream(|_, _| {
        Plan::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"original"}}"#,
        )
    })
    .await;
    let response = invoke(
        &upstream,
        recovery.as_ref(),
        Duration::from_secs(1),
        Method::POST,
        "v1/messages",
        request_headers("Bearer old-managed"),
        Bytes::from_static(b"{}"),
        true,
        Default::default(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(String::from_utf8_lossy(&response_bytes(response).await).contains("gateway OK"));
    assert_eq!(attempts.lock().unwrap().len(), 1);
    assert_eq!(stats.recovered.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn retry_connection_failure_returns_annotated_first_401() {
    let recovering = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (upstream, attempts, server) = spawn_upstream(|_, _| {
        Plan::json(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"original"}}"#,
        )
    })
    .await;
    let recovering_in_future = recovering.clone();
    let release_in_future = release.clone();
    let (recovery, _) = fixture(move || {
        let recovering = recovering_in_future.clone();
        let release = release_in_future.clone();
        Box::pin(async move {
            recovering.notify_one();
            release.notified().await;
            Some(recovered_authorization())
        })
    });
    let request = tokio::spawn({
        let recovery = recovery.clone();
        async move {
            invoke(
                &upstream,
                recovery.as_ref(),
                Duration::from_secs(1),
                Method::POST,
                "v1/messages",
                request_headers("Bearer old-managed"),
                Bytes::from_static(b"{}"),
                true,
                Default::default(),
            )
            .await
        }
    });
    recovering.notified().await;
    server.abort();
    let _ = server.await;
    release.notify_one();
    let response = request.await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(String::from_utf8_lossy(&response_bytes(response).await).contains("gateway OK"));
    assert_eq!(attempts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cancelling_handler_drops_inflight_recovery() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let entered_in_future = entered.clone();
    let dropped_in_future = dropped.clone();
    let (recovery, _) = fixture(move || {
        let entered = entered_in_future.clone();
        let dropped = dropped_in_future.clone();
        Box::pin(async move {
            let _drop = DropMark(dropped);
            entered.notify_one();
            futures_util::future::pending::<Option<HeaderValue>>().await
        })
    });
    let (upstream, _, server) =
        spawn_upstream(|_, _| Plan::json(StatusCode::UNAUTHORIZED, "old")).await;
    let task = tokio::spawn({
        let recovery = recovery.clone();
        async move {
            invoke(
                &upstream,
                recovery.as_ref(),
                Duration::from_secs(60),
                Method::POST,
                "v1/messages",
                request_headers("Bearer old-managed"),
                Bytes::from_static(b"{}"),
                true,
                Default::default(),
            )
            .await
        }
    });
    entered.notified().await;
    task.abort();
    let _ = task.await;
    tokio::task::yield_now().await;
    assert!(dropped.load(Ordering::SeqCst));
    server.abort();
}

#[tokio::test]
async fn first_401_is_not_exposed_before_recovery_finishes() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let entered_in_future = entered.clone();
    let release_in_future = release.clone();
    let (recovery, _) = fixture(move || {
        let entered = entered_in_future.clone();
        let release = release_in_future.clone();
        Box::pin(async move {
            entered.notify_one();
            release.notified().await;
            Some(recovered_authorization())
        })
    });
    let (upstream, _, upstream_server) = spawn_upstream(|index, _| {
        if index == 0 {
            Plan::json(StatusCode::UNAUTHORIZED, "old")
        } else {
            Plan::json(StatusCode::OK, "new")
        }
    })
    .await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let proxy_app = Router::new().route(
        "/p/{pane}/{*rest}",
        any({
            let recovery = recovery.clone();
            let upstream = upstream.clone();
            move |axum::extract::Path((pane, rest)): axum::extract::Path<(String, String)>,
                  method: Method,
                  headers: HeaderMap,
                  body: Bytes| {
                let recovery = recovery.clone();
                let upstream = upstream.clone();
                async move {
                    proxy_handler_inner(
                        Default::default(),
                        reqwest::Client::new(),
                        pane,
                        rest,
                        method,
                        headers,
                        body,
                        true,
                        recovery.as_ref(),
                        ProxyConfig {
                            upstream: &upstream,
                            recovery_timeout: Duration::from_secs(1),
                        },
                    )
                    .await
                }
            }
        }),
    );
    let proxy_server = tokio::spawn(async move {
        axum::serve(listener, proxy_app).await.unwrap();
    });
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("http://{address}/p/12/v1/messages"))
            .header(header::AUTHORIZATION, "Bearer old-managed")
            .body("{}")
            .send()
            .await
            .unwrap()
    });

    entered.notified().await;
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!request.is_finished());
    release.notify_one();
    let response = request.await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "new");
    proxy_server.abort();
    upstream_server.abort();
}

#[test]
fn recovery_path_is_exact() {
    assert!(recovery_path(&Method::POST, "v1/messages"));
    assert!(recovery_path(&Method::POST, "v1/messages/count_tokens"));
    assert!(!recovery_path(&Method::GET, "v1/messages"));
    assert!(!recovery_path(&Method::POST, "v1/messages/"));
    assert!(!recovery_path(&Method::POST, "v1/messages?x=1"));
}

fn _future_is_send(future: impl Future<Output = Response> + Send) {
    drop(future);
}

#[test]
fn proxy_future_remains_send_for_axum() {
    let (recovery, _) = immediate_fixture();
    _future_is_send(invoke(
        "http://127.0.0.1:1",
        recovery.as_ref(),
        Duration::from_millis(1),
        Method::POST,
        "v1/messages",
        request_headers("Bearer old-managed"),
        Bytes::new(),
        true,
        Default::default(),
    ));
}
