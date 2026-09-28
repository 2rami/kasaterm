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

pub(crate) const CONTEXT_HEADER: &str = "x-kasa-uplink-context";
pub(crate) const MACHINES_HEADER: &str = "x-kasa-uplink-machines";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct GatewayMachine {
    pub id: String,
    pub machine: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// 바디 조각 상한 — 한 프레임에 너무 크게 실으면 다른 스트림이 그만큼 기다린다.
pub const CHUNK: usize = 64 * 1024;

/// 스트림 하나가 받는 쪽에서 밀릴 수 있는 프레임 수(최대 CHUNK × 이만큼). 넘치면 그
/// 스트림만 끊는다 — 소켓 수신 루프가 한 스트림을 기다리면 나머지가 다 선다.
pub(crate) const STREAM_QUEUE: usize = 256;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const PING_EVERY: Duration = Duration::from_secs(25);
const PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(75);

type GatewaySocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_gateway(url: &str, timeout: Duration) -> anyhow::Result<GatewaySocket> {
    let (ws, _) = tokio::time::timeout(timeout, tokio_tungstenite::connect_async(url))
        .await.map_err(|_| anyhow::anyhow!("관문 연결 시간이 초과됐어요"))?
        .map_err(|error| anyhow::anyhow!("관문에 못 붙었어요: {error}"))?;
    Ok(ws)
}

async fn exchange_hello(ws: &mut GatewaySocket, hello: String, timeout: Duration) -> anyhow::Result<Option<Result<Message, tokio_tungstenite::tungstenite::Error>>> {
    tokio::time::timeout(timeout, async {
        ws.send(Message::Text(hello.into())).await?;
        Ok(ws.next().await)
    }).await.map_err(|_| anyhow::anyhow!("관문이 hello 에 답이 없어요"))?
}

async fn next_live_frame<S: futures_util::Stream + Unpin>(
    stream: &mut S, last_received: &mut tokio::time::Instant, timeout: Duration,
) -> Result<Option<S::Item>, tokio::time::error::Elapsed> {
    // Settings changes cancel this wait, but only traffic may renew the deadline.
    let frame = tokio::time::timeout_at(*last_received + timeout, stream.next()).await?;
    if frame.is_some() { *last_received = tokio::time::Instant::now(); }
    Ok(frame)
}

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

pub(crate) fn is_internal_header(name: &str) -> bool {
    name.eq_ignore_ascii_case(CONTEXT_HEADER) || name.eq_ignore_ascii_case(MACHINES_HEADER)
}

/// 이 경로가 `/u/<slug>/` 밑에 머무는가. 조각마다 퍼센트 디코딩해 `.`·`..`·경로
/// 구분자·제어문자가 나오면 거절한다.
///
/// 관문은 경로를 디코딩해 넘기고, 받는 쪽은 그것을 URL 로 다시 조립한다 — 그때
/// reqwest(url crate)가 `..` 을 정규화해 `/u/<slug>/../../x` 가 `/x` 가 된다. 접두를
/// 벗어난 loopback 요청은 로컬 권한이라, 손님 주소 하나로 기계 전체가 열렸다.
pub(crate) fn safe_path(raw: &str) -> bool {
    let path = raw.split(['?', '#']).next().unwrap_or("");
    path.split('/').all(|seg| {
        let d = percent_decode(seg);
        d != b"." && d != b".." && !d.iter().any(|&b| b == b'/' || b == b'\\' || b < 0x20 || b == 0x7f)
    })
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn context_token() -> &'static str {
    static TOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOKEN.get_or_init(|| uuid::Uuid::new_v4().simple().to_string())
}

fn encode_machines(machines: &[GatewayMachine]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(machines).unwrap_or_default())
}

/// 공용 관문이 같은 slug/key에서 확인한 살아 있는 기계 목록.
///
/// 두 헤더는 업링크가 loopback 요청을 만들 때만 붙인다. 인터넷이나 로컬 HTTP에서
/// 이름 헤더만 흉내 내도 프로세스 안의 일회성 context 값이 없어 신뢰하지 않는다.
pub(crate) fn verified_machines(headers: &axum::http::HeaderMap) -> Vec<GatewayMachine> {
    use base64::Engine as _;
    let trusted = headers
        .get(CONTEXT_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == context_token());
    if !trusted {
        return Vec::new();
    }
    let Some(encoded) = headers
        .get(MACHINES_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Vec::new();
    };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded) else {
        return Vec::new();
    };
    serde_json::from_slice::<Vec<GatewayMachine>>(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|machine| {
            (8..=128).contains(&machine.id.len())
                && machine
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                && !machine.machine.is_empty()
                && machine.machine.chars().count() <= 80
                && !machine.machine.chars().any(char::is_control)
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub connected: bool,
    pub gateway: Option<String>,
    pub since: Option<Instant>,
    pub last_error: Option<String>,
    /// 관문이 받아 준 slug 수 — 0 이면 주소가 하나도 안 산다(키 충돌 등).
    pub accepted: usize,
    /// 기기 토큰으로 붙었으면 관문이 확인해 준 계정.
    pub account: Option<String>,
    /// 관문이 기기 토큰을 거절했다(폐기·다른 기계) — 다시 로그인해야 한다.
    pub auth_error: Option<String>,
}

/// 관문이 거절한 토큰. 같은 토큰으로 다시 붙으면 또 거절돼 주소(폰)까지 못 쓰게 되니,
/// 새로 로그인할 때까지는 토큰 없이 붙는다.
fn rejected_token() -> &'static Mutex<Option<String>> {
    static R: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    R.get_or_init(|| Mutex::new(None))
}

/// 이번 연결에 실을 기기 토큰.
fn usable_token(gateway: &str) -> Option<String> {
    let token = crate::device_auth::for_gateway(gateway)?.token;
    let rejected = rejected_token().lock().ok()?.clone();
    (rejected.as_deref() != Some(token.as_str())).then_some(token)
}

/// 관문에 붙어 있을 까닭이 있나 — 폰 주소를 열었거나, 기기로 로그인했거나.
fn wanted(gateway: &str) -> bool {
    crate::mobile::published() || usable_token(gateway).is_some()
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

/// 폰 주소를 닫아 둔 채 로그인만 해 둔 기기는 주소 없이 붙는다 — 기기끼리의 길만 쓴다.
fn hello_json(token: Option<&str>) -> Option<String> {
    let key = crate::mobile::machine_key()?;
    let machine_id = crate::mobile::machine_identity()?;
    let slugs: Vec<String> = if crate::mobile::published() {
        // 주인 주소는 여기서 생긴다 — 앱을 처음 켠 사람도 관문에 붙는 순간 주소 하나를 받는다.
        // 안 만들고 빈 목록으로 hello 하면 「붙었는데 주소 0개」가 된다(리그에서 실제로 났다).
        let _ = crate::mobile::owner();
        crate::mobile::users().into_iter().map(|u| u.slug).collect()
    } else {
        Vec::new()
    };
    let mut hello = serde_json::json!({
        "t": "hello",
        "key": key,
        "slugs": slugs,
        "machine": crate::mobile::machine_name(),
        "machine_id": machine_id,
        "machine_aliases": crate::mobile::machine_aliases(),
        "version": env!("CARGO_PKG_VERSION"),
        "proto": 2,
    });
    if let Some(t) = token {
        hello["device_token"] = t.into();
        if crate::mobile::published() {
            if let Some(owner) = crate::mobile::owner() {
                if slugs.contains(&owner.slug) { hello["owner_slug"] = owner.slug.into(); }
            }
        }
    }
    Some(hello.to_string())
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
/// `local_port` 는 업링크 전용 입구(`http.rs` 의 `ViaUplink` 리스너)다 — 본 포트로
/// 되쏘면 관문을 거친 요청이 로컬 CLI 와 구분되지 않는다.
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
        if !wanted(&gateway) {
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
    let mut ws = connect_gateway(&url, CONNECT_TIMEOUT).await?;
    let token = usable_token(gateway);
    let hello = hello_json(token.as_deref()).ok_or_else(|| anyhow::anyhow!("machine_key 를 못 만들었어요"))?;
    // 관문의 첫 답 — ok 가 아니면 이 키로는 못 쓴다(다른 기계가 같은 slug 를 쥐고 있다).
    let first = exchange_hello(&mut ws, hello, HELLO_TIMEOUT).await?;
    let (mut tx, mut rx) = ws.split();
    match first {
        Some(Ok(Message::Text(t))) => {
            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
            if v.get("t").and_then(|x| x.as_str()) != Some("ok") {
                if let Some(t) = &token {
                    token_refused(t, v["error"].as_str().unwrap_or("?"));
                }
                anyhow::bail!("관문이 거절했어요: {t}");
            }
            let n = v.get("accepted").and_then(|a| a.as_array()).map_or(0, |a| a.len());
            let account = v["account"].as_str().map(str::to_string);
            set_status(|s| {
                s.connected = true;
                s.since = Some(Instant::now());
                s.last_error = None;
                s.accepted = n;
                s.account = account.clone();
                if account.is_some() {
                    s.auth_error = None;
                }
            });
            eprintln!(
                "[uplink] {gateway} 에 붙었어요 — 주소 {n}개{}",
                account.map(|a| format!(", 계정 {a}")).unwrap_or_default()
            );
        }
        other => anyhow::bail!("관문의 첫 답이 이상해요: {other:?}"),
    }
    // 앱→관문 쓰기는 한 태스크로 — 스트림 여럿이 한 소켓을 나눠 쓴다.
    let (wtx, mut wrx) = mpsc::channel::<Message>(256);
    let mut writer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(PING_EVERY);
        tick.tick().await;
        loop {
            tokio::select! {
                m = wrx.recv() => match m {
                    Some(m) => if !matches!(tokio::time::timeout(PEER_IDLE_TIMEOUT, tx.send(m)).await, Ok(Ok(()))) { break },
                    None => break,
                },
                // 터널·프록시의 유휴 끊김(~100초)을 앞질러 간다.
                _ = tick.tick() => if !matches!(tokio::time::timeout(PEER_IDLE_TIMEOUT, tx.send(Message::Ping(Vec::new().into()))).await, Ok(Ok(()))) { break },
            }
        }
    });
    let streams: Arc<Mutex<std::collections::HashMap<u32, mpsc::Sender<(u8, Vec<u8>)>>>> =
        Arc::new(Mutex::new(Default::default()));
    let mut last_received = tokio::time::Instant::now();
    let result: anyhow::Result<()> = loop {
        tokio::select! {
            _ = &mut writer => break Err(anyhow::anyhow!("관문으로 보내는 연결이 끊겼어요")),
            _ = poke_notify().notified() => {
                // 껐거나·관문이 바뀌었거나·로그인이 바뀌었다 — 바깥 루프가 새로 붙는다.
                if !wanted(gateway)
                    || crate::mobile::gateway().as_deref() != Some(gateway)
                    || usable_token(gateway) != token
                {
                    break Ok(());
                }
                if let Some(h) = hello_json(token.as_deref()) {
                    if wtx.try_send(Message::Text(h.into())).is_err() {
                        break Err(anyhow::anyhow!("관문으로 보내는 연결이 지연되고 있어요"));
                    }
                }
            }
            received = next_live_frame(&mut rx, &mut last_received, PEER_IDLE_TIMEOUT) => {
                let m = match received {
                    Ok(frame) => frame,
                    Err(_) => break Err(anyhow::anyhow!("관문 응답이 끊겼어요 — 다시 연결합니다")),
                };
                match m {
                Some(Ok(Message::Binary(b))) => {
                    let Some((kind, id, payload)) = decode(&b) else { continue };
                    if kind == OPEN {
                        let (stx, srx) = mpsc::channel::<(u8, Vec<u8>)>(STREAM_QUEUE);
                        streams.lock().unwrap().insert(id, stx);
                        let open: serde_json::Value = match serde_json::from_slice(payload) {
                            Ok(v) => v,
                            Err(_) => {
                                streams.lock().unwrap().remove(&id);
                                let _ = wtx.try_send(Message::Binary(encode(CLOSE, id, b"bad open").into()));
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
                    // 모르는 스트림(이미 끝난 것)이면 조용히 버린다 — CLOSE 를 되쏘면 핑퐁이 된다.
                    if let Some(tx) = tx {
                        match tx.try_send((kind, payload.to_vec())) {
                            Ok(()) => {}
                            // 기다리지 않고 그 스트림만 끊는다 — 여기서 기다리면 같은 소켓의
                            // 다른 스트림이 전부 선다.
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                streams.lock().unwrap().remove(&id);
                                let _ = wtx.try_send(Message::Binary(encode(CLOSE, id, b"overflow").into()));
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                streams.lock().unwrap().remove(&id);
                            }
                        }
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
                            if let Some(t) = &token {
                                token_refused(t, &why);
                            }
                            break Err(anyhow::anyhow!("관문 오류: {why}"));
                        }
                        _ => {}
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    if wtx.try_send(Message::Pong(p)).is_err() {
                        break Err(anyhow::anyhow!("관문으로 보내는 연결이 지연되고 있어요"));
                    }
                }
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Close(_))) | None => break Err(anyhow::anyhow!("관문이 연결을 닫았어요")),
                Some(Err(e)) => break Err(anyhow::anyhow!("관문 연결 유실: {e}")),
                Some(Ok(_)) => {}
                }
            }
        }
    };
    set_status(|s| {
        s.connected = false;
        s.accepted = 0;
        s.account = None;
    });
    streams.lock().unwrap().clear();
    writer.abort();
    result
}

/// 관문이 기기 토큰을 받지 않았다. 폐기·다른 기계 토큰이면 새로 로그인할 때까지 토큰 없이
/// 붙는다 — 그래야 폰 주소는 계속 산다.
fn token_refused(token: &str, why: &str) {
    if !matches!(why, "device_token_invalid" | "device_revoked") {
        return;
    }
    crate::device_auth::reject_token(token);
    if let Ok(mut r) = rejected_token().lock() {
        *r = Some(token.to_string());
    }
    set_status(|s| s.auth_error = Some("관문 로그인이 풀렸어요 — 다시 로그인해 주세요".into()));
    eprintln!("[uplink] 관문이 이 기기의 로그인을 받지 않았어요({why}) — 토큰 없이 다시 붙어요");
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
    let gateway_machines: Vec<GatewayMachine> = open
        .get("machines")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
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
    // 옛 관문은 경로를 디코딩해 넘긴다 — 새 관문이 막더라도 여기서 한 번 더 본다.
    let prefix = format!("{}{slug}/", crate::mobile::PREFIX);
    let stays_under_slug = crate::mobile::valid_slug(slug)
        && safe_path(path)
        && reqwest::Url::parse(&format!("http://{local}")).is_ok_and(|u| u.path().starts_with(&prefix));
    if !stays_under_slug {
        send(HEAD, br#"{"status":400,"headers":[]}"#.to_vec()).await;
        send(if is_ws { CLOSE } else { END }, Vec::new()).await;
        return;
    }
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
            if is_internal_header(k) {
                continue;
            }
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
        for (k, v) in [(CONTEXT_HEADER, context_token().to_string()), (MACHINES_HEADER, encode_machines(&gateway_machines))] {
            if let Ok(val) = axum::http::HeaderValue::from_str(&v) {
                req.headers_mut().insert(k, val);
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
    // HTTP — 요청 바디는 BODY…END 로 흘러오고, 그대로 로컬로 흘려 보낸다. END 뒤에도
    // 스트림을 계속 들어 CLOSE(폰이 떠남)를 잡는다 — 안 그러면 끝없는 응답(SSE·롱폴)이
    // 아무도 안 읽는 채로 계속 흐른다.
    let (btx, brx) = mpsc::channel::<Result<Vec<u8>, std::io::Error>>(32);
    let (closed_tx, mut closed_rx) = tokio::sync::oneshot::channel::<()>();
    let feeder = tokio::spawn(async move {
        let mut btx = Some(btx);
        while let Some((kind, p)) = srx.recv().await {
            match kind {
                BODY => {
                    if let Some(b) = &btx {
                        if b.send(Ok(p)).await.is_err() {
                            btx = None;
                        }
                    }
                }
                END => btx = None,
                CLOSE => break,
                _ => {}
            }
        }
        let _ = closed_tx.send(());
    });
    let body = reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(brx));
    let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let client = client();
    let mut rb = client.request(method, format!("http://{local}")).body(body);
    for (k, v) in &headers {
        if skip_header(k) || is_internal_header(k) {
            continue;
        }
        rb = rb.header(k.as_str(), v.as_str());
    }
    rb = rb
        .header(CONTEXT_HEADER, context_token())
        .header(MACHINES_HEADER, encode_machines(&gateway_machines));
    let sent = tokio::select! {
        r = rb.send() => r,
        _ = &mut closed_rx => {
            feeder.abort();
            return;
        }
    };
    let resp = match sent {
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
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = &mut closed_rx => {
                feeder.abort();
                return;
            }
        };
        match chunk {
            Some(Ok(b)) => {
                for part in b.chunks(CHUNK) {
                    if !send(BODY, part.to_vec()).await {
                        feeder.abort();
                        return;
                    }
                }
            }
            Some(Err(_)) | None => break,
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

    #[tokio::test]
    async fn stalled_websocket_handshake_has_a_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let result = tokio::time::timeout(Duration::from_secs(2), connect_gateway(&url, Duration::from_millis(40))).await;
        server.abort();
        let error = result.expect("WebSocket handshake never timed out").unwrap_err();
        assert!(error.to_string().contains("연결 시간이 초과"));
    }

    #[tokio::test]
    async fn stalled_hello_response_has_a_deadline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            assert!(matches!(ws.next().await, Some(Ok(Message::Text(_)))));
            std::future::pending::<()>().await;
        });
        let mut ws = connect_gateway(&url, Duration::from_secs(2)).await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2),
            exchange_hello(&mut ws, "test hello".into(), Duration::from_millis(40))).await;
        server.abort();
        assert!(result.expect("hello never timed out").unwrap_err().to_string().contains("hello"));
    }

    #[tokio::test]
    async fn inbound_frames_refresh_liveness_but_cancelled_waits_do_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (tx, mut commands) = mpsc::channel::<Message>(8);
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(frame) = commands.recv().await { ws.send(frame).await.unwrap(); }
        });
        let mut ws = connect_gateway(&url, Duration::from_secs(2)).await.unwrap();
        let mut received_at = tokio::time::Instant::now() - Duration::from_millis(1);
        for frame in [Message::Text("ok".into()), Message::Binary(vec![1, 2].into()), Message::Ping(vec![3].into())] {
            let before = received_at;
            tx.send(frame.clone()).await.unwrap();
            let received = next_live_frame(&mut ws, &mut received_at, Duration::from_secs(1)).await.unwrap().unwrap().unwrap();
            assert_eq!(received, frame);
            assert!(received_at > before);
        }
        let before = received_at;
        let cancelled = tokio::time::timeout(Duration::from_millis(10),
            next_live_frame(&mut ws, &mut received_at, Duration::from_secs(1))).await;
        assert!(cancelled.is_err());
        assert_eq!(received_at, before);
        received_at = tokio::time::Instant::now() - Duration::from_secs(1);
        let result = tokio::time::timeout(Duration::from_secs(2),
            next_live_frame(&mut ws, &mut received_at, Duration::from_millis(40))).await;
        server.abort();
        assert!(result.expect("silent connection never timed out").is_err());
    }

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

    #[test]
    fn safe_path_rejects_segments_that_climb_out() {
        for ok in ["/", "/hub", "/term/ws", "/m/%EB%AF%B8%EB%8B%88/term/panes", "/a.b/c..d", "/x?y=../z"] {
            assert!(safe_path(ok), "{ok}");
        }
        for bad in ["/../x", "/%2e%2e/x", "/%2E%2e/x", "/.%2e/x", "/./x", "/%2e/x", "/a%2f..%2fb", "/a%5cb", "/a%00b", "/a\\b"] {
            assert!(!safe_path(bad), "{bad}");
        }
    }

    const SLUG: &str = "abcdefghijklmnopqrstuvwxy";

    async fn next_frame(wrx: &mut mpsc::Receiver<Message>) -> (u8, Vec<u8>) {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), wrx.recv()).await.expect("frame").expect("open");
            if let Message::Binary(b) = m {
                let (k, _, p) = decode(&b).unwrap();
                return (k, p.to_vec());
            }
        }
    }

    #[tokio::test]
    async fn paths_that_leave_the_user_address_never_reach_the_app() {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h2 = hits.clone();
        let app = axum::Router::new().fallback(move || {
            h2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { "local" }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let server = tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        for path in ["/%2e%2e/%2e%2e/version", "/../../version"] {
            let (_stx, srx) = mpsc::channel(STREAM_QUEUE);
            let (wtx, mut wrx) = mpsc::channel(STREAM_QUEUE);
            let open = serde_json::json!({ "slug": SLUG, "path": path, "method": "GET", "headers": [] });
            tokio::spawn(handle_stream(1, open, srx, wtx, port));
            let (kind, head) = next_frame(&mut wrx).await;
            assert_eq!(kind, HEAD);
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&head).unwrap()["status"], 400, "{path}");
        }
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn phone_leaving_stops_an_endless_response() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = dropped.clone();
        let app = axum::Router::new().fallback(move || {
            let flag = Dropped(d2.clone());
            async move {
                let s = futures_util::stream::unfold(flag, |flag| async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Some((Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"tick")), flag))
                });
                axum::body::Body::from_stream(s)
            }
        });
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let server = tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (stx, srx) = mpsc::channel(STREAM_QUEUE);
        let (wtx, mut wrx) = mpsc::channel(STREAM_QUEUE);
        let open = serde_json::json!({ "slug": SLUG, "path": "/events", "method": "GET", "headers": [] });
        let task = tokio::spawn(handle_stream(7, open, srx, wtx, port));
        stx.send((END, Vec::new())).await.unwrap();
        assert_eq!(next_frame(&mut wrx).await.0, HEAD);
        assert_eq!(next_frame(&mut wrx).await.0, BODY);
        stx.send((CLOSE, Vec::new())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), task).await.expect("폰이 떠났는데 응답을 계속 흘렸다").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "로컬 서버의 끝없는 응답이 정리되지 않았다");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        server.abort();
    }

    #[test]
    fn machine_context_rejects_spoofed_headers() {
        let machines = vec![
            GatewayMachine {
                id: "machine-book-1".to_string(),
                machine: "맥북".to_string(),
                aliases: vec!["geno".to_string()],
            },
            GatewayMachine {
                id: "machine-mini-1".to_string(),
                machine: "미니".to_string(),
                aliases: vec!["nachoneko".to_string()],
            },
        ];
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(MACHINES_HEADER, encode_machines(&machines).parse().unwrap());
        assert!(verified_machines(&headers).is_empty(), "이름 헤더만으로 통과했다");

        headers.insert(CONTEXT_HEADER, "fake".parse().unwrap());
        assert!(verified_machines(&headers).is_empty(), "가짜 context가 통과했다");

        headers.insert(CONTEXT_HEADER, context_token().parse().unwrap());
        assert_eq!(verified_machines(&headers), machines);
    }
}
