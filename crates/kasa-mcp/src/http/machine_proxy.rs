//! 다른 기계로 넘기는 문 `/m/<기계>/<경로>` 와 나쵸 앱 중계.
//!
//! 폰 주소 하나(`/u/<slug>/`)로 **명부(machines.json)의 기계 전부**를 보게 한다 —
//! 기계마다 터널을 파지 않는다. 자격은 관문이 이미 봤으니(slug·토큰·로컬) 여기선
//! 옮기기만 한다. HTTP 와 WebSocket 둘 다.

use super::*;

pub(super) const PROXY_BODY_LIMIT: usize = 64 * 1024 * 1024;

fn proxy_client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().build().expect("reqwest client"))
}

/// 대상 기계로 **넘기지 않는** 헤더. hop-by-hop 과, 대상의 관문을 엉뚱하게 자극할 것들:
/// 쿠키·Origin·sec-fetch 는 대상 입장에서 남의 사이트처럼 보이고, X-Forwarded-For 는
/// 대상이 「원격이니 토큰 내라」고 막는다(터널 안쪽은 양끝 loopback 이라 토큰이 없다).
fn proxy_skip_request_header(k: &axum::http::HeaderName) -> bool {
    if crate::uplink::is_internal_header(k.as_str()) {
        return true;
    }
    matches!(
        k.as_str(),
        "host"
            | "connection"
            | "upgrade"
            | "cookie"
            | "origin"
            | "referer"
            | "content-length"
            | "transfer-encoding"
            | "accept-encoding"
            | "x-kasa-token"
            | "x-forwarded-for"
            | "x-forwarded-proto"
            | "x-forwarded-host"
            | "cf-connecting-ip"
    ) || k.as_str().starts_with("sec-")
}

/// 폰으로 **되돌려주지 않는** 헤더. 대상이 심는 토큰 쿠키는 이 기계 것과 달라서
/// 그대로 흘리면 우리 쿠키를 덮어 절대경로 fetch 가 죽는다.
fn proxy_skip_response_header(k: &axum::http::HeaderName) -> bool {
    matches!(k.as_str(), "set-cookie" | "connection" | "upgrade" | "transfer-encoding")
}

async fn machine_proxy(
    AxPath((label, rest)): AxPath<(String, String)>,
    req: axum::extract::Request,
) -> axum::response::Response {
    // 대상 기계에는 이 기계의 터널로 닿아 **로컬 권한**이 된다 — 주인만 건너가게 한다.
    if req.extensions().get::<MobileAuth>().is_some_and(|auth| !auth.0.owner) {
        return (axum::http::StatusCode::FORBIDDEN, "owner only").into_response();
    }
    let Some(m) = crate::machines::find_route(&label) else {
        return (axum::http::StatusCode::NOT_FOUND, "no such machine").into_response();
    };
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = format!("{}/{rest}{query}", m.base);
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if is_ws {
        if !ws_origin_ok(req.headers()) {
            return (axum::http::StatusCode::FORBIDDEN, "cross-origin websocket refused").into_response();
        }
        use axum::extract::FromRequestParts as _;
        let (mut parts, _body) = req.into_parts();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(w) => w,
            Err(e) => return e.into_response(),
        };
        let ws_target = if let Some(rest) = target.strip_prefix("https://") {
            format!("wss://{rest}")
        } else {
            format!("ws://{}", target.trim_start_matches("http://"))
        };
        return ws
            .on_upgrade(move |sock| proxy_ws(sock, ws_target, label))
            .into_response();
    }
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, PROXY_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response()
        }
    };
    let mut rb = proxy_client().request(parts.method.clone(), &target);
    for (k, v) in parts.headers.iter() {
        if !proxy_skip_request_header(k) {
            rb = rb.header(k, v);
        }
    }
    let resp = match rb.body(bytes).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("{label} 에 못 닿았어요: {e}"),
            )
                .into_response()
        }
    };
    let mut out = axum::response::Response::builder().status(resp.status());
    for (k, v) in resp.headers().iter() {
        if !proxy_skip_response_header(k) {
            out = out.header(k, v);
        }
    }
    use futures_util::TryStreamExt as _;
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    out.body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|_| axum::http::StatusCode::BAD_GATEWAY.into_response())
}

/// 카사모바일 → 나쵸 앱 창구. 신원은 이 요청의 `MobileAuth`(주소로 확인한 사용자)만 믿는다 —
/// 폰이 실어 보낸 헤더는 `nacho_relay` 가 하나도 옮기지 않는다.
async fn nacho_app_relay(
    AxPath(rest): AxPath<String>,
    req: axum::extract::Request,
) -> axum::response::Response {
    let user = req.extensions().get::<MobileAuth>().map(|a| a.0.clone());
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, crate::nacho_relay::BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };
    crate::nacho_relay::relay(user, parts.method, &rest, parts.uri.query(), &parts.headers, bytes).await
}

/// 키 없는 기기 데스크톱의 모드·권한 표 읽기(`nacho_relay::read_relay`). 판정 재료는 여기서만 뽑는다 —
/// 원격(전달 헤더 포함)인가, 폰 주소로 왔나. 호출자 헤더·몸통은 넘기지 않는다.
async fn nacho_read_relay(AxPath(name): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let remote = is_remote_peer(&req);
    let phone = req.extensions().get::<MobileAuth>().is_some();
    crate::nacho_relay::read_relay(req.method(), &name, remote, phone).await
}

/// 폰 ↔ 이 기계 ↔ 대상 기계의 WS 를 양방향으로 잇는다. Ping/Pong 도 **그대로 옮긴다** —
/// 대상 서버는 Pong 이 75초 없으면 피어가 잠든 것으로 보고 끊는데(`term_ws_run`),
/// tungstenite 의 자동 pong 은 다음 쓰기 때까지 안 나가서 그 판정에 걸린다.
async fn proxy_ws(sock: WebSocket, url: String, label: String) {
    use axum::extract::ws::Message as AM;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TM;
    let (mut ctx, mut crx) = sock.split();
    let up = match tokio_tungstenite::connect_async(&url).await {
        Ok((u, _)) => u,
        Err(e) => {
            eprintln!("[m-proxy] {label} WS 연결 실패: {e}");
            // 클라가 「기계가 죽었다」와 「잠깐 끊겼다」를 가르게 — gone 은 재접속을 멈춘다.
            let _ = ctx
                .send(AM::Text(
                    serde_json::json!({ "t": "gone", "why": "machine unreachable" })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let (mut utx, mut urx) = up.split();
    loop {
        tokio::select! {
            m = crx.next() => match m {
                Some(Ok(AM::Binary(b))) => if utx.send(TM::Binary(b)).await.is_err() { break },
                Some(Ok(AM::Text(t))) => if utx.send(TM::Text(t.as_str().into())).await.is_err() { break },
                Some(Ok(AM::Ping(p))) => if utx.send(TM::Ping(p)).await.is_err() { break },
                Some(Ok(AM::Pong(p))) => if utx.send(TM::Pong(p)).await.is_err() { break },
                Some(Ok(AM::Close(_))) | Some(Err(_)) | None => break,
            },
            m = urx.next() => match m {
                Some(Ok(TM::Binary(b))) => if ctx.send(AM::Binary(b)).await.is_err() { break },
                Some(Ok(TM::Text(t))) => if ctx.send(AM::Text(t.as_str().into())).await.is_err() { break },
                Some(Ok(TM::Ping(p))) => if ctx.send(AM::Ping(p)).await.is_err() { break },
                Some(Ok(TM::Pong(p))) => if ctx.send(AM::Pong(p)).await.is_err() { break },
                Some(Ok(TM::Frame(_))) => {}
                Some(Ok(TM::Close(_))) | Some(Err(_)) | None => break,
            },
        }
    }
    let _ = utx.close().await;
    let _ = ctx.close().await;
}

/// 이 모듈 창구의 라우트. `router` 가 한 표로 합친 뒤 공통 레이어(Origin·토큰 가드)를 두른다.
pub(super) fn routes() -> axum::Router {
    axum::Router::new()
        .route("/m/{label}/{*rest}", axum::routing::any(machine_proxy))
        .route("/nacho/app/{*rest}", axum::routing::any(nacho_app_relay))
        .route("/nacho/read/{*name}", axum::routing::any(nacho_read_relay))
}
