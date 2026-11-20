//! 화면 스트림 — 계약과 흐름은 `kasa-cast`, 여기는 이 앱 프로세스에 꽂는 자리(카사넷 입구·폰 웹소켓·기기 base 로 보기).
//! 계약·실측은 `docs/screen-stream.md`, 앱 밖 서비스로 떼는 길은 `docs/screen-service-split.md`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use kasa_net::iroh::endpoint::Connection;
use serde_json::{json, Value};
use tokio::sync::mpsc;

pub use kasa_cast::{
    audio, ctl, now, run_watch, serve_conn, serve_conn_audio, spawn_watch, Capture, Config, Denied, Event, Frame, Input,
    Inputs, Keyframes, Rate, Session, Watch, BACKLOG_MAX, SCREEN_ALPN,
};

static CAPTURE: OnceLock<Arc<dyn Capture>> = OnceLock::new();

/// 이 판이 화면을 압축해 보낼 수 있나 — 화면 거울 소켓의 영상 방식을 받을지 가른다.
pub fn installed() -> bool {
    CAPTURE.get().is_some()
}

/// 꽂은 보내기 — 폰 웹소켓·옛 보는 쪽 중계가 같은 것을 쓴다.
pub fn capture() -> Option<Arc<dyn Capture>> {
    CAPTURE.get().cloned()
}

/// 앱이 부팅 때 한 번 — 카사넷 화면 입구도 함께 연다.
pub fn install(capture: Arc<dyn Capture>) {
    if CAPTURE.set(capture).is_ok() {
        kasa_device::kasanet::set_screen_handler(Arc::new(|conn| Box::pin(serve(conn))));
    }
}

/// 보는 쪽 하나와의 연결. 보는 쪽마다 따로 떠서 설정을 제각각 고른다.
pub async fn serve(conn: Connection) {
    serve_conn(conn, CAPTURE.get().cloned()).await;
}

/// 화면 거울 소켓의 영상 방식(`/screen/ws?codec=hevc`, `docs/screen-stream.md` S2) — 폰이 카사넷 직통일 때 JPEG 대신 받는다.
/// 첫 말은 화면 거울과 같은 `hello` 에 `codec`·장 크기를 더한 것, 장은 바이너리 `'F'` 틀(카사넷 장과 같다). 보는 쪽은
/// `{"t":"keyframe"}` 과 화면 거울 조작 JSON 을 보낸다. 사정은 화면 거울처럼 `{"t":"denied"}`·`{"t":"input"}`.
pub async fn serve_ws(socket: axum::extract::ws::WebSocket, config: Config) {
    serve_ws_with(socket, CAPTURE.get().cloned(), config).await;
}

pub(crate) async fn serve_ws_with(socket: axum::extract::ws::WebSocket, capture: Option<Arc<dyn Capture>>, config: Config) {
    use axum::extract::ws::Message;
    use futures_util::{SinkExt, StreamExt};
    let text = |v: Value| Message::Text(v.to_string().into());
    let (mut tx, mut rx) = socket.split();
    let Some(capture) = capture else {
        let _ = tx.send(text(json!({ "t": "denied", "reason": "unavailable" }))).await;
        let _ = tx.close().await;
        return;
    };
    let (frames_tx, mut frames) = mpsc::unbounded_channel::<Frame>();
    let started = {
        let (capture, config) = (capture.clone(), config.clone());
        tokio::task::spawn_blocking(move || capture.start(&config, Box::new(move |f| drop(frames_tx.send(f))))).await
    };
    let session = match started {
        Ok(Ok(s)) => s,
        Ok(Err(denied)) => {
            if denied == Denied::Permission {
                let c = capture.clone();
                let _ = tokio::task::spawn_blocking(move || c.request_permission()).await;
            }
            let reason = if denied == Denied::Permission { "permission" } else { "unavailable" };
            let _ = tx.send(text(json!({ "t": "denied", "reason": reason }))).await;
            let _ = tx.close().await;
            return;
        }
        Err(_) => return,
    };
    let info = session.info();
    let display = session.display();
    let (w, h) = (info.get("width").cloned().unwrap_or(Value::Null), info.get("height").cloned().unwrap_or(Value::Null));
    let hello = json!({
        "t": "hello", "codec": config.codec, "info": info, "display": display,
        "displays": [{ "id": display, "width": w, "height": h, "main": true }],
    });
    if tx.send(text(hello)).await.is_err() {
        return;
    }
    let mut input = kasa_cast::Inputs::default();
    let mut waiting_key = false;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + Duration::from_secs(20), Duration::from_secs(20));
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                // 보내지 못한 장이 쌓이면 다 버리고 키프레임부터 — 카사넷 장과 같은 규칙.
                if frames.len() >= BACKLOG_MAX {
                    while frames.try_recv().is_ok() {}
                    session.keyframe();
                    waiting_key = true;
                    continue;
                }
                if waiting_key && !frame.key {
                    continue;
                }
                waiting_key = false;
                if tx.send(Message::Binary(frame.encode().into())).await.is_err() {
                    break;
                }
            }
            msg = rx.next() => {
                let raw = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => continue,
                };
                if serde_json::from_str::<Value>(raw.as_str()).ok().and_then(|v| v.get("t")?.as_str().map(str::to_string)).as_deref() == Some("keyframe") {
                    session.keyframe();
                    continue;
                }
                let Some(event) = kasa_cast::mirror::input_of(raw.as_str()) else { continue };
                if let Some(mut told) = input.put(&capture, display, event).await {
                    told["t"] = json!("input");
                    if tx.send(text(told)).await.is_err() {
                        break;
                    }
                }
            }
            _ = ping.tick() => {
                if tx.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        }
    }
    input.release(&capture, display).await;
    let _ = tokio::task::spawn_blocking(move || drop(session)).await;
    let _ = tx.close().await;
}

/// base 의 기기 화면을 본다. 카사넷 직통·믿는 중계가 아니면 바로 `Closed` 로 끝난다.
pub fn watch(base: String, config: Config, on: impl Fn(Event) + Send + Sync + 'static) -> Option<Watch> {
    let rt = kasa_device::kasanet::runtime()?;
    let connect = async move { kasa_device::kasanet::connect(&base, SCREEN_ALPN).await };
    Some(spawn_watch(&rt, connect, config, on))
}

#[cfg(test)]
mod tests;
