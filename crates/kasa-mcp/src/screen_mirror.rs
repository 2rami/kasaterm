//! 기기 화면 거울 — 이 기기의 화면을 JPEG 로 떠서 폰·다른 기기의 카사텀에 흘린다(`docs/screen-mirror.md`).
//!
//! 화면을 뜨는 법은 OS 마다 달라 앱이 [`install`] 로 꽂는다. 관문·`kasa-device` 처럼 화면이 없는 곳은 꽂지 않아
//! `unavailable` 로 답한다. 여기는 소켓·역압·뜨는 박자만 맡는다.
//!
//! - 서버 → 보는 쪽: 붙자마자 텍스트 `{"t":"hello",displays,display}`, 그 뒤 바이너리
//!   `[u32 seq BE][u16 meta 길이 BE][meta JSON {w,h,d}][JPEG]`. 뜰 수 없으면 `{"t":"denied","reason"}`.
//! - 보는 쪽 → 서버: 그림을 다 그린 뒤 `{"t":"ack","seq"}`, 설정을 바꿀 때 `{"t":"tune",display?,w?,h?,q?,fps?}`.
//! - ack 안 받은 프레임은 [`WINDOW`] 장까지만 보내고, 막힌 동안은 화면을 뜨지도 않는다. 관문의 흐름 큐(256 프레임)가
//!   느린 폰 때문에 차서 끊기는 일이 없고, 아무도 안 보는 화면을 뜨느라 CPU 를 쓰지 않는다.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::json;

/// ack 없이 보낼 수 있는 프레임 수.
const WINDOW: u32 = 2;
/// 화면이 이만큼 안 바뀌면 덜 자주 떠 본다 — 멈춘 화면을 초당 여러 번 떠 봐야 같은 그림이다.
const IDLE_AFTER: Duration = Duration::from_secs(2);
const IDLE_EVERY: Duration = Duration::from_millis(500);
const PING_EVERY: Duration = Duration::from_secs(20);
/// 이만큼 아무 말(ack·핑·퐁)이 없으면 반쯤 열린 소켓으로 보고 닫는다.
const SILENT_LIMIT: Duration = Duration::from_secs(60);
/// 권한이 없을 때 다시 떠 보는 간격 — 그 기기 앞의 사람이 설정에서 켜는 것을 기다린다.
const DENIED_RETRY: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Display {
    pub id: u32,
    /// 화면 좌표(포인트) 크기. 레티나면 픽셀은 이보다 크다.
    pub width: u32,
    pub height: u32,
    pub main: bool,
}

pub struct Shot {
    /// 줄인 픽셀의 지문. 다음 [`Ask::prev`] 로 돌아와 같은 그림을 다시 인코딩하지 않게 한다.
    pub hash: u64,
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// macOS 화면 기록 권한이 없다. 권한 없이도 뜨이기는 하지만 배경화면만 담겨 거짓 화면이 된다.
    Permission,
    Unavailable,
}

impl Denied {
    fn reason(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    pub display: u32,
    pub max_w: u32,
    pub max_h: u32,
    pub quality: u8,
    pub prev: u64,
}

pub trait Source: Send + Sync {
    fn displays(&self) -> Vec<Display>;
    /// 비율을 지켜 `max_w × max_h` 안으로 줄여 뜬다(키우지는 않는다). 픽셀이 `ask.prev` 와 같으면 `Ok(None)`.
    fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied>;
    /// 그 기기 앞에 권한 창을 띄운다. OS 는 앱마다 처음 한 번만 띄우므로 연결마다 한 번만 부른다.
    fn request_permission(&self) {}
}

static SOURCE: OnceLock<Arc<dyn Source>> = OnceLock::new();

pub fn install(source: Arc<dyn Source>) {
    let _ = SOURCE.set(source);
}

pub(crate) fn installed() -> Option<Arc<dyn Source>> {
    SOURCE.get().cloned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tune {
    display: Option<u32>,
    max_w: u32,
    max_h: u32,
    quality: u8,
    fps: u32,
}

impl Default for Tune {
    fn default() -> Self {
        Self { display: None, max_w: 1600, max_h: 1600, quality: 60, fps: 8 }
    }
}

impl Tune {
    /// 쿼리(글)와 `tune` 메시지(수)가 같은 이름을 쓴다. 범위 밖 값은 끝으로 붙인다.
    pub(crate) fn apply(&mut self, get: impl Fn(&str) -> Option<u64>) {
        let clamp = |v: u64, lo: u32, hi: u32| v.clamp(u64::from(lo), u64::from(hi)) as u32;
        if let Some(d) = get("display") {
            self.display = u32::try_from(d).ok();
        }
        if let Some(w) = get("w") {
            self.max_w = clamp(w, 160, 3840);
        }
        if let Some(h) = get("h") {
            self.max_h = clamp(h, 160, 3840);
        }
        if let Some(q) = get("q") {
            self.quality = clamp(q, 20, 90) as u8;
        }
        if let Some(fps) = get("fps") {
            self.fps = clamp(fps, 1, 15);
        }
    }

    fn every(&self) -> Duration {
        Duration::from_millis(1000 / u64::from(self.fps.max(1)))
    }
}

fn pick(displays: &[Display], want: Option<u32>) -> u32 {
    want.filter(|w| displays.iter().any(|d| d.id == *w))
        .or_else(|| displays.iter().find(|d| d.main).map(|d| d.id))
        .or_else(|| displays.first().map(|d| d.id))
        .unwrap_or(0)
}

fn frame(seq: u32, shot: &Shot, display: u32) -> Vec<u8> {
    let meta = json!({ "w": shot.width, "h": shot.height, "d": display }).to_string();
    let mut out = Vec::with_capacity(6 + meta.len() + shot.jpeg.len());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&(meta.len() as u16).to_be_bytes());
    out.extend_from_slice(meta.as_bytes());
    out.extend_from_slice(&shot.jpeg);
    out
}

fn text(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

enum Control {
    Ack(u32),
    Tune(serde_json::Value),
}

fn control(raw: &str) -> Option<Control> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    match v.get("t")?.as_str()? {
        "ack" => Some(Control::Ack(u32::try_from(v.get("seq")?.as_u64()?).ok()?)),
        "tune" => Some(Control::Tune(v)),
        _ => None,
    }
}

async fn displays_of(source: &Arc<dyn Source>) -> Vec<Display> {
    let source = source.clone();
    tokio::task::spawn_blocking(move || source.displays()).await.unwrap_or_default()
}

/// 보는 쪽 하나와의 소켓. 보는 쪽마다 따로 떠서 크기·품질을 제각각 고른다.
pub(crate) async fn serve(socket: WebSocket, source: Option<Arc<dyn Source>>, mut tune: Tune) {
    let (mut tx, mut rx) = socket.split();
    let Some(source) = source else {
        let _ = tx.send(text(json!({ "t": "denied", "reason": Denied::Unavailable.reason() }))).await;
        let _ = tx.close().await;
        return;
    };
    let mut displays = displays_of(&source).await;
    let mut display = pick(&displays, tune.display);
    if tx.send(text(json!({ "t": "hello", "displays": displays, "display": display }))).await.is_err() {
        return;
    }
    let (mut seq, mut acked, mut prev) = (0u32, 0u32, 0u64);
    let mut next = Instant::now();
    let mut changed_at = Instant::now();
    let mut heard = Instant::now();
    let mut denied: Option<Denied> = None;
    let mut asked = false;
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    loop {
        let open = seq.wrapping_sub(acked) < WINDOW;
        tokio::select! {
            m = rx.next() => {
                let Some(Ok(m)) = m else { break };
                heard = Instant::now();
                let raw = match m {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };
                match control(raw.as_str()) {
                    // 보낸 적 없는 번호나 이미 받은 것보다 옛 번호는 버린다 — 창을 거짓으로 열면 쌓인다.
                    Some(Control::Ack(n)) if seq.wrapping_sub(n) < seq.wrapping_sub(acked) => acked = n,
                    Some(Control::Tune(v)) => {
                        tune.apply(|k| v.get(k).and_then(serde_json::Value::as_u64));
                        if tune.display.is_some_and(|d| d != display) {
                            displays = displays_of(&source).await;
                            display = pick(&displays, tune.display);
                            let hello = json!({ "t": "hello", "displays": displays, "display": display });
                            if tx.send(text(hello)).await.is_err() {
                                break;
                            }
                        }
                        prev = 0;
                        next = Instant::now();
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep_until(next.into()), if open => {
                let started = Instant::now();
                let ask = Ask { display, max_w: tune.max_w, max_h: tune.max_h, quality: tune.quality, prev };
                let s = source.clone();
                let Ok(got) = tokio::task::spawn_blocking(move || s.capture(&ask)).await else { break };
                match got {
                    Ok(Some(shot)) => {
                        seq = seq.wrapping_add(1);
                        if tx.send(Message::Binary(frame(seq, &shot, display).into())).await.is_err() {
                            break;
                        }
                        prev = shot.hash;
                        changed_at = Instant::now();
                        denied = None;
                    }
                    Ok(None) => {}
                    Err(why) => {
                        if denied != Some(why) {
                            denied = Some(why);
                            if tx.send(text(json!({ "t": "denied", "reason": why.reason() }))).await.is_err() {
                                break;
                            }
                        }
                        if why == Denied::Permission && !asked {
                            asked = true;
                            let s = source.clone();
                            let _ = tokio::task::spawn_blocking(move || s.request_permission()).await;
                        }
                        prev = 0;
                        next = Instant::now() + DENIED_RETRY;
                        continue;
                    }
                }
                let every = if changed_at.elapsed() > IDLE_AFTER { IDLE_EVERY.max(tune.every()) } else { tune.every() };
                next = started + every;
            }
            _ = ping.tick() => {
                if heard.elapsed() > SILENT_LIMIT || tx.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = tx.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio_tungstenite::tungstenite::Message as TM;

    /// 지문이 바깥에서 정해지는 가짜 화면. `hash` 를 바꾸면 「화면이 바뀌었다」.
    struct Fake {
        hash: AtomicU64,
        denied: Mutex<Option<Denied>>,
        captures: AtomicUsize,
        asked: AtomicUsize,
        last: Mutex<Option<Ask>>,
    }

    impl Fake {
        fn new() -> Arc<Fake> {
            Arc::new(Fake {
                hash: AtomicU64::new(1),
                denied: Mutex::new(None),
                captures: AtomicUsize::new(0),
                asked: AtomicUsize::new(0),
                last: Mutex::new(None),
            })
        }
    }

    impl Source for Fake {
        fn displays(&self) -> Vec<Display> {
            vec![
                Display { id: 7, width: 1512, height: 982, main: false },
                Display { id: 9, width: 2560, height: 1440, main: true },
            ]
        }

        fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            *self.last.lock().unwrap() = Some(*ask);
            if let Some(why) = *self.denied.lock().unwrap() {
                return Err(why);
            }
            let hash = self.hash.load(Ordering::SeqCst);
            if hash == ask.prev {
                return Ok(None);
            }
            Ok(Some(Shot { hash, jpeg: vec![0xFF, 0xD8, hash as u8], width: 320, height: 200 }))
        }

        fn request_permission(&self) {
            self.asked.fetch_add(1, Ordering::SeqCst);
        }
    }

    type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    async fn connect(source: Option<Arc<Fake>>, tune: Tune) -> Client {
        let source = source.map(|s| s as Arc<dyn Source>);
        let app = axum::Router::new().route(
            "/screen/ws",
            axum::routing::get(move |ws: axum::extract::WebSocketUpgrade| {
                let source = source.clone();
                async move { ws.on_upgrade(move |socket| serve(socket, source, tune)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/screen/ws")).await.unwrap();
        ws
    }

    async fn next(ws: &mut Client, wait: Duration) -> Option<TM> {
        loop {
            match tokio::time::timeout(wait, ws.next()).await {
                Ok(Some(Ok(TM::Ping(_) | TM::Pong(_)))) => continue,
                Ok(Some(Ok(m))) => return Some(m),
                _ => return None,
            }
        }
    }

    async fn json_of(ws: &mut Client) -> serde_json::Value {
        match next(ws, Duration::from_secs(2)).await {
            Some(TM::Text(t)) => serde_json::from_str(t.as_str()).unwrap(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    fn seq_of(m: &TM) -> u32 {
        let TM::Binary(b) = m else { panic!("expected frame, got {m:?}") };
        let meta_len = u16::from_be_bytes([b[4], b[5]]) as usize;
        let meta: serde_json::Value = serde_json::from_slice(&b[6..6 + meta_len]).unwrap();
        assert_eq!(meta["w"], 320);
        assert_eq!(&b[6 + meta_len..6 + meta_len + 2], &[0xFF, 0xD8]);
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    async fn ack(ws: &mut Client, seq: u32) {
        ws.send(TM::Text(json!({ "t": "ack", "seq": seq }).to_string().into())).await.unwrap();
    }

    fn fast() -> Tune {
        Tune { fps: 15, ..Tune::default() }
    }

    #[tokio::test]
    async fn hello_names_the_main_display_unless_one_is_asked() {
        let fake = Fake::new();
        let mut ws = connect(Some(fake.clone()), fast()).await;
        let hello = json_of(&mut ws).await;
        assert_eq!(hello["t"], "hello");
        assert_eq!(hello["display"], 9);
        assert_eq!(hello["displays"].as_array().unwrap().len(), 2);

        let mut ws = connect(Some(fake), Tune { display: Some(7), ..fast() }).await;
        assert_eq!(json_of(&mut ws).await["display"], 7);
    }

    #[tokio::test]
    async fn never_more_than_the_window_unacked() {
        let fake = Fake::new();
        let mut ws = connect(Some(fake.clone()), fast()).await;
        json_of(&mut ws).await;
        let mut seqs = Vec::new();
        for n in 2..=12u64 {
            fake.hash.store(n, Ordering::SeqCst);
            if let Some(m) = next(&mut ws, Duration::from_millis(150)).await {
                seqs.push(seq_of(&m));
            }
        }
        assert_eq!(seqs, vec![1, 2], "ack 없이 창을 넘겨 보냈다");
        let before = fake.captures.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(fake.captures.load(Ordering::SeqCst), before, "막힌 동안에도 화면을 떴다");

        ack(&mut ws, 1).await;
        fake.hash.store(99, Ordering::SeqCst);
        let m = next(&mut ws, Duration::from_secs(1)).await.expect("ack 뒤 프레임");
        assert_eq!(seq_of(&m), 3);
        assert!(next(&mut ws, Duration::from_millis(300)).await.is_none());
    }

    #[tokio::test]
    async fn a_future_ack_does_not_open_the_window() {
        let fake = Fake::new();
        let mut ws = connect(Some(fake.clone()), fast()).await;
        json_of(&mut ws).await;
        assert_eq!(seq_of(&next(&mut ws, Duration::from_secs(1)).await.unwrap()), 1);
        fake.hash.store(2, Ordering::SeqCst);
        assert_eq!(seq_of(&next(&mut ws, Duration::from_secs(1)).await.unwrap()), 2);
        ack(&mut ws, 40).await;
        fake.hash.store(3, Ordering::SeqCst);
        assert!(next(&mut ws, Duration::from_millis(400)).await.is_none());
    }

    #[tokio::test]
    async fn an_unchanged_screen_sends_nothing_more() {
        let fake = Fake::new();
        let mut ws = connect(Some(fake.clone()), fast()).await;
        json_of(&mut ws).await;
        let first = next(&mut ws, Duration::from_secs(1)).await.unwrap();
        ack(&mut ws, seq_of(&first)).await;
        assert!(next(&mut ws, Duration::from_millis(400)).await.is_none());
        assert_eq!(fake.last.lock().unwrap().unwrap().prev, 1, "지난 지문을 넘기지 않았다");
    }

    #[tokio::test]
    async fn tune_switches_display_and_resends_the_screen() {
        let fake = Fake::new();
        let mut ws = connect(Some(fake.clone()), fast()).await;
        json_of(&mut ws).await;
        let first = next(&mut ws, Duration::from_secs(1)).await.unwrap();
        ack(&mut ws, seq_of(&first)).await;
        let tune = json!({ "t": "tune", "display": 7, "w": 900, "h": 99_999, "q": 5 });
        ws.send(TM::Text(tune.to_string().into())).await.unwrap();
        let hello = json_of(&mut ws).await;
        assert_eq!(hello["display"], 7);
        let m = next(&mut ws, Duration::from_secs(1)).await.expect("바꾼 뒤 같은 그림이라도 다시 보낸다");
        assert_eq!(seq_of(&m), 2);
        let ask = fake.last.lock().unwrap().unwrap();
        assert_eq!((ask.display, ask.max_w, ask.max_h, ask.quality), (7, 900, 3840, 20));
    }

    #[tokio::test]
    async fn denied_is_told_once_and_permission_asked_once() {
        let fake = Fake::new();
        *fake.denied.lock().unwrap() = Some(Denied::Permission);
        let mut ws = connect(Some(fake.clone()), fast()).await;
        json_of(&mut ws).await;
        let denied = json_of(&mut ws).await;
        assert_eq!((denied["t"].as_str(), denied["reason"].as_str()), (Some("denied"), Some("permission")));
        assert!(next(&mut ws, Duration::from_millis(300)).await.is_none());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_source_says_unavailable() {
        let mut ws = connect(None, fast()).await;
        let denied = json_of(&mut ws).await;
        assert_eq!(denied["reason"], "unavailable");
    }

    #[test]
    fn pick_falls_back_to_main_then_first() {
        let ds = Fake::new().displays();
        assert_eq!(pick(&ds, Some(7)), 7);
        assert_eq!(pick(&ds, Some(3)), 9);
        assert_eq!(pick(&ds[..1], None), 7);
        assert_eq!(pick(&[], None), 0);
    }
}
