use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kasa_cast::testing::{Counts, FakeCapture};
use serde_json::{json, Value};

use super::*;

const T: Duration = Duration::from_secs(20);
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TM;

type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(capture: Option<Arc<dyn Capture>>) -> Client {
    let config = Config { fps: 30, ..Config::default() };
    let app = axum::Router::new().route(
        "/screen/ws",
        axum::routing::get(move |up: axum::extract::ws::WebSocketUpgrade| {
            let (capture, config) = (capture.clone(), config.clone());
            async move { up.on_upgrade(move |s| serve_ws_with(s, capture, config)) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio_tungstenite::connect_async(format!("ws://{addr}/screen/ws")).await.unwrap().0
}

async fn next(ws: &mut Client) -> TM {
    loop {
        match tokio::time::timeout(T, ws.next()).await.unwrap().unwrap().unwrap() {
            TM::Ping(_) | TM::Pong(_) => continue,
            m => return m,
        }
    }
}

async fn next_frame(ws: &mut Client) -> Frame {
    loop {
        if let TM::Binary(b) = next(ws).await {
            return Frame::decode(&b).expect("장 틀");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn video_socket_says_hello_then_streams_frames_from_a_keyframe_and_takes_input() {
    let counts = Arc::new(Counts::default());
    let mut ws = connect(Some(Arc::new(FakeCapture { counts: counts.clone(), deny: None, deny_input: None }))).await;
    let TM::Text(hello) = next(&mut ws).await else { panic!("첫 말은 hello") };
    let hello: Value = serde_json::from_str(&hello).unwrap();
    assert_eq!((hello["t"].as_str(), hello["codec"].as_str(), hello["display"].as_u64()), (Some("hello"), Some("hevc"), Some(7)));
    assert_eq!(hello["displays"][0]["width"], 64, "장 크기를 화면 크기로 알린다");
    let first = next_frame(&mut ws).await;
    assert!(first.key && !first.param_sets.is_empty());
    let second = next_frame(&mut ws).await;
    assert_eq!(second.seq, first.seq + 1);
    ws.send(TM::Text(json!({ "t": "keyframe" }).to_string().into())).await.unwrap();
    ws.send(TM::Text(json!({ "t": "pointer", "x": 0.5, "y": 0.5, "b": 1 }).to_string().into())).await.unwrap();
    let told = loop {
        if let TM::Text(t) = next(&mut ws).await {
            break serde_json::from_str::<Value>(&t).unwrap();
        }
    };
    assert_eq!(told, json!({ "t": "input", "ok": true }), "화면 거울과 같은 사정 말");
    assert_eq!(counts.keyframes.load(Ordering::Relaxed), 1);
    ws.close(None).await.unwrap();
    let waited = Instant::now();
    while counts.stopped.load(Ordering::Relaxed) == 0 && waited.elapsed() < T {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(counts.stopped.load(Ordering::Relaxed), 1, "닫으면 뜨기를 멈춘다");
    let ups = counts.inputs.lock().unwrap().clone();
    assert_eq!(ups.last().map(|(_, i)| i.clone()), Some(Input::Pointer { x: 0.5, y: 0.5, buttons: 0, prev: 1, mods: 0 }), "떠날 때 단추를 뗀다");
}

#[tokio::test(flavor = "multi_thread")]
async fn video_socket_without_capture_or_permission_says_why() {
    let mut ws = connect(None).await;
    let TM::Text(t) = next(&mut ws).await else { panic!() };
    assert_eq!(serde_json::from_str::<Value>(&t).unwrap(), json!({ "t": "denied", "reason": "unavailable" }));
    let counts = Arc::new(Counts::default());
    let mut ws = connect(Some(Arc::new(FakeCapture { counts: counts.clone(), deny: Some(Denied::Permission), deny_input: None }))).await;
    let TM::Text(t) = next(&mut ws).await else { panic!() };
    assert_eq!(serde_json::from_str::<Value>(&t).unwrap(), json!({ "t": "denied", "reason": "permission" }));
    assert_eq!(counts.prompts.load(Ordering::Relaxed), 1);
}
