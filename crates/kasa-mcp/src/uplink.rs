//! 업링크 — kasaterm 이 공용 관문(중계소)에 **스스로 붙어** 자기 폰 주소를 받는 길.
//!
//! 「유저마다 주소 하나」는 **카사텀을 쓰는 사람마다 앱이 저절로 자기 주소를 받는 것**
//! 이다(2026-09-02 정정 「주소만들기가 아니라 각자 카사텀 쓰는 사람마다 생성되게」).
//! 사람마다 터널을 파거나 포트를 열 수는 없으니, 앱이 바깥의 관문에 WebSocket 하나를
//! 걸어 두고, 폰이 `https://<관문>/u/<slug>/…` 로 오면 관문이 그 요청을 이 소켓으로
//! 내려보내고 앱이 **자기 로컬 HTTP 서버**에 대신 물어 답을 올려보낸다. 관문은 주소를
//! 기계로 잇기만 하고, 자격·화면·pane 은 전부 이 앱의 `mobile.rs`·`http.rs` 그대로다.
//!
//! 소켓 하나에 요청 여럿을 싣는 **다중화 프레임**(바이너리):
//! `[kind u8][stream u32 BE][payload]` — kind 는 아래 상수. 텍스트 프레임은 제어
//! (`hello`·`ok`·`err`). 관문 쪽 구현은 `gateway.rs`, 여기는 프레임 규약과 **앱 쪽**.
//!
//! 자격: 앱은 `machine_key`(mobile-users.json, 기계마다 하나) 와 자기 slug 목록으로
//! `hello` 한다. 관문은 slug 를 처음 본 키에 묶고, 다른 키가 같은 slug 를 대면 거절한다
//! — 남의 주소를 가로채 자기 기계로 끌어오지 못하게. 이 앱은 코드 0줄이 더 필요 없다:
//! 관문이 내려보낸 요청을 `http://127.0.0.1:<포트>/u/<slug>/…` 로 되쏘면 로컬 관문
//! (`mobile_prefix_mw`)이 slug 로 자격을 매기고 MobileAuth 를 심는다.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message;

pub const OPEN: u8 = 1; // 관문→앱. JSON {slug, method, path, headers:[[k,v]], ws}
pub const HEAD: u8 = 2; // 앱→관문. JSON {status, headers:[[k,v]]}
pub const BODY: u8 = 3; // 양방향 바디 조각
pub const END: u8 = 4; // 이 방향의 바디 끝
pub const WS_TEXT: u8 = 5;
pub const WS_BIN: u8 = 6;
pub const WS_PING: u8 = 7;
pub const WS_PONG: u8 = 8;
pub const CLOSE: u8 = 9; // 스트림 종료(어느 쪽이든). payload = 사유(utf8, 선택)

/// 바디 조각 상한 — 한 프레임에 너무 크게 실으면 다른 스트림이 그만큼 기다린다.
pub const CHUNK: usize = 64 * 1024;

pub fn encode(kind: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(kind);
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn decode(b: &[u8]) -> Option<(u8, u32, &[u8])> {
    if b.len() < 5 {
        return None;
    }
    let id = u32::from_be_bytes([b[1], b[2], b[3], b[4]]);
    Some((b[0], id, &b[5..]))
}

/// 관문↔앱 사이에서 넘기지 않는 헤더. hop-by-hop 과 길이류(다시 계산된다).
pub fn skip_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "upgrade"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "te"
            | "trailer"
            | "proxy-connection"
    ) || name.starts_with("sec-websocket-")
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub connected: bool,
    pub gateway: Option<String>,
    pub since: Option<Instant>,
    pub last_error: Option<String>,
    /// 관문이 받아 준 slug 수 — 0 이면 주소가 하나도 안 산다(키 충돌 등).
    pub accepted: usize,
}

fn state() -> &'static Mutex<Status> {
    static S: std::sync::OnceLock<Mutex<Status>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(Status::default()))
}

pub fn status() -> Status {
    state().lock().map(|s| s.clone()).unwrap_or_default()
}

fn poke_notify() -> &'static Notify {
    static N: std::sync::OnceLock<Notify> = std::sync::OnceLock::new();
    N.get_or_init(Notify::new)
}

/// 설정이 바뀌었다(켜짐/꺼짐·유저 추가). 붙어 있으면 hello 를 다시 보내고, 꺼졌으면 끊는다.
pub fn poke() {
    poke_notify().notify_waiters();
}

fn set_status(f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = state().lock() {
        f(&mut s);
    }
}

fn hello_json() -> Option<String> {
    let key = crate::mobile::machine_key()?;
    // 주인 주소는 여기서 생긴다 — 앱을 처음 켠 사람도 관문에 붙는 순간 주소 하나를 받는다.
    // 안 만들고 빈 목록으로 hello 하면 「붙었는데 주소 0개」가 된다(리그에서 실제로 났다).
    let _ = crate::mobile::owner();
    let slugs: Vec<String> = crate::mobile::users().into_iter().map(|u| u.slug).collect();
    Some(
        serde_json::json!({
            "t": "hello",
            "key": key,
            "slugs": slugs,
            "machine": crate::mobile::machine_name(),
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    )
}

fn ws_url(gateway: &str) -> String {
    let g = gateway.trim_end_matches('/');
    let g = if let Some(r) = g.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = g.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        format!("wss://{g}")
    };
    format!("{g}/relay/uplink")
}

/// 앱 부팅 때 한 번. 관문이 설정돼 있는 동안 붙고, 끊기면 백오프로 다시 붙는다.
pub fn spawn(local_port: u16) {
    tokio::spawn(run(local_port));
}

async fn run(local_port: u16) {
    let mut backoff = Duration::from_secs(1);
    // 연결 주소가 둘(gateway_connect · gateway)이면 실패마다 번갈아 든다 — ssh 터널이
    // 죽어 있으면 공용 주소로, 공용이 막혀 있으면 터널로.
    let mut attempt: u32 = 0;
    loop {
        let Some(gateway) = crate::mobile::gateway() else {
            set_status(|s| {
                s.connected = false;
                s.gateway = None;
            });
            poke_notify().notified().await;
            continue;
        };
        if !crate::mobile::published() {
            set_status(|s| {
                s.connected = false;
                s.gateway = Some(gateway.clone());
            });
            poke_notify().notified().await;
            continue;
        }
        set_status(|s| s.gateway = Some(gateway.clone()));
        let preferred = crate::mobile::gateway_connect().unwrap_or_else(|| gateway.clone());
        let connect = if attempt % 2 == 0 || preferred == gateway { preferred } else { gateway.clone() };
        attempt = attempt.wrapping_add(1);
        match session(&gateway, &connect, local_port).await {
            Ok(()) => backoff = Duration::from_secs(1),
            Err(e) => {
                eprintln!("[uplink] {e}");
                set_status(|s| {
                    s.connected = false;
                    s.last_error = Some(e.to_string());
                });
            }
        }
        // 꺼짐·설정 변경이면 바로, 아니면 백오프 뒤 다시.
        tokio::select! {
            _ = poke_notify().notified() => {}
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

/// 연결 하나의 수명. Ok = 정상 종료(끄기·설정 변경), Err = 연결 실패·유실.
async fn session(gateway: &str, connect: &str, local_port: u16) -> anyhow::Result<()> {
    let url = ws_url(connect);
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| anyhow::anyhow!("관문 {url} 에 못 붙었어요: {e}"))?;
    let (mut tx, mut rx) = ws.split();
    let hello = hello_json().ok_or_else(|| anyhow::anyhow!("machine_key 를 못 만들었어요"))?;
    tx.send(Message::Text(hello.into())).await?;
    // 관문의 첫 답 — ok 가 아니면 이 키로는 못 쓴다(다른 기계가 같은 slug 를 쥐고 있다).
    let first = tokio::time::timeout(Duration::from_secs(15), rx.next())
        .await
        .map_err(|_| anyhow::anyhow!("관문이 hello 에 답이 없어요"))?;
    match first {
        Some(Ok(Message::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
            if v.get("t").and_then(|x| x.as_str()) != Some("ok") {
                anyhow::bail!("관문이 거절했어요: {t}");
            }
            let n = v.get("accepted").and_then(|a| a.as_array()).map_or(0, |a| a.len());
            set_status(|s| {
                s.connected = true;
                s.since = Some(Instant::now());
                s.last_error = None;
                s.accepted = n;
            });
            eprintln!("[uplink] {gateway} 에 붙었어요 — 주소 {n}개");
        }
        other => anyhow::bail!("관문의 첫 답이 이상해요: {other:?}"),
    }
    // 앱→관문 쓰기는 한 태스크로 — 스트림 여럿이 한 소켓을 나눠 쓴다.
    let (wtx, mut wrx) = mpsc::channel::<Message>(256);
    let writer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(25));
        tick.tick().await;
        loop {
            tokio::select! {
                m = wrx.recv() => match m {
                    Some(m) => if tx.send(m).await.is_err() { break },
                    None => break,
                },
                // 터널·프록시의 유휴 끊김(~100초)을 앞질러 간다.
                _ = tick.tick() => if tx.send(Message::Ping(Vec::new().into())).await.is_err() { break },
            }
        }
    });
    let streams: Arc<Mutex<std::collections::HashMap<u32, mpsc::Sender<(u8, Vec<u8>)>>>> =
        Arc::new(Mutex::new(Default::default()));
    let result: anyhow::Result<()> = loop {
        tokio::select! {
            _ = poke_notify().notified() => {
                if !crate::mobile::published() || crate::mobile::gateway().as_deref() != Some(gateway) {
                    break Ok(()); // 껐거나 관문이 바뀌었다 — 바깥 루프가 다시 정한다
                }
                if let Some(h) = hello_json() {
                    let _ = wtx.send(Message::Text(h.into())).await; // 유저가 늘었다 — 다시 알린다
                }
            }
            m = rx.next() => match m {
                Some(Ok(Message::Binary(b))) => {
                    let Some((kind, id, payload)) = decode(&b) else { continue };
                    if kind == OPEN {
                        let (stx, srx) = mpsc::channel::<(u8, Vec<u8>)>(64);
                        streams.lock().unwrap().insert(id, stx);
                        let open: serde_json::Value = match serde_json::from_slice(payload) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = wtx.send(Message::Binary(encode(CLOSE, id, b"bad open").into())).await;
                                continue;
                            }
                        };
                        let wtx2 = wtx.clone();
                        let streams2 = streams.clone();
                        tokio::spawn(async move {
                            handle_stream(id, open, srx, wtx2.clone(), local_port).await;
                            streams2.lock().unwrap().remove(&id);
                        });
                        continue;
                    }
                    let tx = streams.lock().unwrap().get(&id).cloned();
                    match tx {
                        Some(tx) => {
                            if tx.send((kind, payload.to_vec())).await.is_err() {
                                streams.lock().unwrap().remove(&id);
                            }
                        }
                        // 모르는 스트림(이미 끝난 것)이면 조용히 버린다 — CLOSE 를 되쏘면 핑퐁이 된다.
                        None => {}
                    }
                }
                Some(Ok(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                    match v.get("t").and_then(|x| x.as_str()) {
                        Some("ok") => {
                            let n = v.get("accepted").and_then(|a| a.as_array()).map_or(0, |a| a.len());
                            set_status(|s| s.accepted = n);
                        }
                        Some("err") => {
                            let why = v.get("error").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                            break Err(anyhow::anyhow!("관문 오류: {why}"));
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Ping(p))) => { let _ = wtx.send(Message::Pong(p)).await; }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break Err(anyhow::anyhow!("관문이 연결을 닫았어요")),
                Some(Err(e)) => break Err(anyhow::anyhow!("관문 연결 유실: {e}")),
                Some(Ok(_)) => {}
            }
        }
    };
    set_status(|s| {
        s.connected = false;
        s.accepted = 0;
    });
    writer.abort();
    result
}

/// 관문이 내려보낸 요청 하나. 로컬 서버에 `/u/<slug>/<경로>` 로 되쏘고 답을 올려보낸다.
async fn handle_stream(
    id: u32,
    open: serde_json::Value,
    mut srx: mpsc::Receiver<(u8, Vec<u8>)>,
    wtx: mpsc::Sender<Message>,
    local_port: u16,
) {
    let slug = open.get("slug").and_then(|x| x.as_str()).unwrap_or("");
    let path = open.get("path").and_then(|x| x.as_str()).unwrap_or("/");
    let method = open.get("method").and_then(|x| x.as_str()).unwrap_or("GET");
    let is_ws = open.get("ws").and_then(|x| x.as_bool()).unwrap_or(false);
    let headers: Vec<(String, String)> = open
        .get("headers")
        .and_then(|h| h.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|kv| {
                    let k = kv.get(0)?.as_str()?.to_string();
                    let v = kv.get(1)?.as_str()?.to_string();
                    Some((k, v))
                })
                .collect()
        })
        .unwrap_or_default();
    // 로컬 관문(`mobile_prefix_mw`)이 slug 로 자격을 매긴다 — 여기서 토큰을 붙일 일이 없다.
    let local = format!(
        "127.0.0.1:{local_port}{}{slug}{path}",
        crate::mobile::PREFIX
    );
    let send = |kind: u8, payload: Vec<u8>| {
        let wtx = wtx.clone();
        async move { wtx.send(Message::Binary(encode(kind, id, &payload).into())).await.is_ok() }
    };
    if is_ws {
        let url = format!("ws://{local}");
        let mut req = match tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url.as_str()) {
            Ok(r) => r,
            Err(_) => {
                send(HEAD, br#"{"status":502,"headers":[]}"#.to_vec()).await;
                send(CLOSE, b"bad url".to_vec()).await;
                return;
            }
        };
        for (k, v) in &headers {
            // Origin 은 안 넘긴다 — 로컬 `ws_origin_ok` 는 Origin 이 있으면 Host 와 같기를 요구한다.
            if k == "origin" || k == "cookie" || skip_header(k) {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                axum::http::HeaderName::from_bytes(k.as_bytes()),
                axum::http::HeaderValue::from_str(v),
            ) {
                req.headers_mut().insert(name, val);
            }
        }
        let (local_ws, _) = match tokio_tungstenite::connect_async(req).await {
            Ok(x) => x,
            Err(e) => {
                let _ = send(HEAD, format!(r#"{{"status":502,"headers":[["x-kasa-error",{:?}]]}}"#, e.to_string()).into_bytes()).await;
                send(CLOSE, b"local ws failed".to_vec()).await;
                return;
            }
        };
        send(HEAD, br#"{"status":101,"headers":[]}"#.to_vec()).await;
        let (mut ltx, mut lrx) = local_ws.split();
        loop {
            tokio::select! {
                f = srx.recv() => match f {
                    Some((WS_TEXT, p)) => {
                        let s = String::from_utf8_lossy(&p).to_string();
                        if ltx.send(Message::Text(s.into())).await.is_err() { break }
                    }
                    Some((WS_BIN, p)) => if ltx.send(Message::Binary(p.into())).await.is_err() { break },
                    Some((WS_PING, p)) => if ltx.send(Message::Ping(p.into())).await.is_err() { break },
                    Some((WS_PONG, p)) => if ltx.send(Message::Pong(p.into())).await.is_err() { break },
                    Some((CLOSE, _)) | None => break,
                    Some(_) => {}
                },
                m = lrx.next() => match m {
                    Some(Ok(Message::Text(t))) => if !send(WS_TEXT, t.as_str().as_bytes().to_vec()).await { break },
                    Some(Ok(Message::Binary(b))) => if !send(WS_BIN, b.to_vec()).await { break },
                    Some(Ok(Message::Ping(p))) => if !send(WS_PING, p.to_vec()).await { break },
                    Some(Ok(Message::Pong(p))) => if !send(WS_PONG, p.to_vec()).await { break },
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
        let _ = ltx.close().await;
        send(CLOSE, Vec::new()).await;
        return;
    }
    // HTTP — 요청 바디는 BODY…END 로 흘러오고, 그대로 로컬로 흘려 보낸다.
    let (btx, brx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
    let mut srx_for_body = srx;
    let feeder = tokio::spawn(async move {
        let mut ended = false;
        while let Some((kind, p)) = srx_for_body.recv().await {
            match kind {
                BODY => {
                    if btx.send(Ok(p)).await.is_err() {
                        break;
                    }
                }
                END => {
                    ended = true;
                    break;
                }
                CLOSE => break,
                _ => {}
            }
        }
        (ended, srx_for_body)
    });
    let body = reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(brx));
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = client();
    let mut rb = client.request(method, format!("http://{local}")).body(body);
    for (k, v) in &headers {
        if skip_header(k) {
            continue;
        }
        rb = rb.header(k.as_str(), v.as_str());
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            let _ = send(HEAD, format!(r#"{{"status":502,"headers":[["x-kasa-error",{:?}]]}}"#, e.to_string()).into_bytes()).await;
            send(END, Vec::new()).await;
            feeder.abort();
            return;
        }
    };
    let hs: Vec<serde_json::Value> = resp
        .headers()
        .iter()
        .filter(|(k, _)| !skip_header(k.as_str()))
        .filter_map(|(k, v)| Some(serde_json::json!([k.as_str(), v.to_str().ok()?])))
        .collect();
    let head = serde_json::json!({ "status": resp.status().as_u16(), "headers": hs }).to_string();
    if !send(HEAD, head.into_bytes()).await {
        feeder.abort();
        return;
    }
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(b) => {
                for part in b.chunks(CHUNK) {
                    if !send(BODY, part.to_vec()).await {
                        feeder.abort();
                        return;
                    }
                }
            }
            Err(_) => break,
        }
    }
    send(END, Vec::new()).await;
    feeder.abort();
}

fn client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().build().expect("reqwest client"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let f = encode(BODY, 0x0102_0304, b"hi");
        let (k, id, p) = decode(&f).unwrap();
        assert_eq!((k, id, p), (BODY, 0x0102_0304, &b"hi"[..]));
        assert!(decode(&[1, 2]).is_none());
    }

    #[test]
    fn ws_url_maps_scheme() {
        assert_eq!(ws_url("https://kasaterm.example.com/"), "wss://kasaterm.example.com/relay/uplink");
        assert_eq!(ws_url("http://127.0.0.1:8791"), "ws://127.0.0.1:8791/relay/uplink");
        assert_eq!(ws_url("kasaterm.example.com"), "wss://kasaterm.example.com/relay/uplink");
    }

    #[test]
    fn hop_headers_are_skipped() {
        for h in ["host", "connection", "content-length", "sec-websocket-key", "transfer-encoding"] {
            assert!(skip_header(h), "{h}");
        }
        for h in ["content-type", "accept", "cookie", "x-kasa-token"] {
            assert!(!skip_header(h), "{h}");
        }
    }
}
