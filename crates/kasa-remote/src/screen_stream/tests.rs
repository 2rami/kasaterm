use std::net::Ipv4Addr;
use std::sync::atomic::AtomicUsize;

use kasa_net::iroh::endpoint::{presets, PortmapperConfig};
use kasa_net::iroh::protocol::{AcceptError, ProtocolHandler, Router};
use kasa_net::iroh::{Endpoint, RelayMode, SecretKey};

use super::*;

const T: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Counts {
    started: AtomicUsize,
    stopped: AtomicUsize,
    keyframes: AtomicUsize,
    prompts: AtomicUsize,
    input_prompts: AtomicUsize,
    inputs: Mutex<Vec<(u32, Input)>>,
}

/// 5ms 마다 장을 낸다. 첫 장·청한 다음 장이 키프레임이다.
struct FakeCapture {
    counts: Arc<Counts>,
    deny: Option<Denied>,
    deny_input: Option<Denied>,
}

struct FakeSession {
    counts: Arc<Counts>,
    stop: Arc<AtomicBool>,
    want_key: Arc<AtomicBool>,
}

impl Capture for FakeCapture {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        if let Some(d) = self.deny {
            return Err(d);
        }
        assert_eq!(config.fps, 30, "보는 쪽 설정이 그대로 온다");
        self.counts.started.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let want_key = Arc::new(AtomicBool::new(true));
        let (s, k) = (stop.clone(), want_key.clone());
        std::thread::spawn(move || {
            let mut seq = 0u32;
            while !s.load(Ordering::Relaxed) {
                seq += 1;
                let key = k.swap(false, Ordering::Relaxed);
                out(Frame {
                    seq,
                    cap_id: u64::from(seq),
                    key,
                    t_cap: 1.0,
                    t_enc: 1.001,
                    width: 64,
                    height: 32,
                    param_sets: if key { vec![vec![1, 2], vec![3]] } else { Vec::new() },
                    data: vec![seq as u8; 100],
                });
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Box::new(FakeSession { counts: self.counts.clone(), stop, want_key }))
    }

    fn request_permission(&self) {
        self.counts.prompts.fetch_add(1, Ordering::Relaxed);
    }

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        if let Some(d) = self.deny_input {
            return Err(d);
        }
        self.counts.inputs.lock().unwrap().push((display, input.clone()));
        Ok(())
    }

    fn request_input_permission(&self) {
        self.counts.input_prompts.fetch_add(1, Ordering::Relaxed);
    }

    fn now(&self) -> f64 {
        now() + 100.0
    }
}

impl Session for FakeSession {
    fn keyframe(&self) {
        self.counts.keyframes.fetch_add(1, Ordering::Relaxed);
        self.want_key.store(true, Ordering::Relaxed);
    }

    fn info(&self) -> Value {
        json!({ "width": 64, "height": 32 })
    }

    fn display(&self) -> u32 {
        7
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.counts.stopped.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct Host(Arc<dyn Capture>);

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Host")
    }
}

impl ProtocolHandler for Host {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        serve_conn(conn, Some(self.0.clone())).await;
        Ok(())
    }
}

async fn node() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// 보내는 쪽 라우터와 보는 쪽 연결, 그리고 보는 쪽 사건을 받는 채널.
/// 보는 쪽 엔드포인트도 함께 돌려준다 — 버리면 연결이 닫힌다.
async fn session(capture: FakeCapture) -> (Router, Endpoint, mpsc::UnboundedReceiver<Event>, mpsc::UnboundedSender<(u8, Vec<u8>)>, tokio::task::JoinHandle<String>) {
    let host = node().await;
    let router = Router::builder(host.clone()).accept(SCREEN_ALPN, Host(Arc::new(capture))).spawn();
    let viewer = node().await;
    let conn = tokio::time::timeout(T, viewer.connect(host.addr(), SCREEN_ALPN)).await.unwrap().unwrap();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
    let config = Config { fps: 30, ..Config::default() };
    let run = tokio::spawn(run_watch(conn, config, Arc::new(move |e| drop(ev_tx.send(e))), ctl_rx));
    (router, viewer, ev_rx, ctl_tx, run)
}

async fn next_frame(rx: &mut mpsc::UnboundedReceiver<Event>) -> (Frame, f64) {
    loop {
        match tokio::time::timeout(T, rx.recv()).await.unwrap().unwrap() {
            Event::Frame { frame, offset, .. } => return (frame, offset),
            Event::Closed(why) => panic!("닫힘: {why}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_arrive_in_order_from_a_keyframe_and_closing_stops_capture() {
    let counts = Arc::new(Counts::default());
    let (_router, _viewer, mut rx, ctl, run) = session(FakeCapture { counts: counts.clone(), deny: None, deny_input: None }).await;
    let (first, offset) = next_frame(&mut rx).await;
    assert!(first.key && first.param_sets == vec![vec![1, 2], vec![3]], "첫 장은 파라미터 셋을 실은 키프레임");
    assert!((offset - 100.0).abs() < 0.05, "시계 차이를 장이 흐르기 전에 잡는다: {offset}");
    let mut last = first.seq;
    for _ in 0..10 {
        let (f, _) = next_frame(&mut rx).await;
        assert_eq!(f.seq, last + 1, "장은 순서대로, 빠짐없이");
        assert_eq!(f.data, vec![f.seq as u8; 100]);
        last = f.seq;
    }
    ctl.send((ctl::KEYFRAME, Vec::new())).unwrap();
    let key = loop {
        let (f, _) = next_frame(&mut rx).await;
        if f.key {
            break f;
        }
    };
    assert!(!key.param_sets.is_empty());
    assert_eq!(counts.keyframes.load(Ordering::Relaxed), 1);
    ctl.send((ctl::QUIT, Vec::new())).unwrap();
    assert_eq!(tokio::time::timeout(T, run).await.unwrap().unwrap(), "닫음");
    let waited = Instant::now();
    while counts.stopped.load(Ordering::Relaxed) == 0 && waited.elapsed() < T {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!((counts.started.load(Ordering::Relaxed), counts.stopped.load(Ordering::Relaxed)), (1, 1), "닫으면 뜨기를 멈춘다");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_capture_without_permission_says_so_and_asks_once() {
    let counts = Arc::new(Counts::default());
    let (_router, _viewer, mut rx, _ctl, run) = session(FakeCapture { counts: counts.clone(), deny: Some(Denied::Permission), deny_input: None }).await;
    let why = tokio::time::timeout(T, run).await.unwrap().unwrap();
    assert_eq!(why, "거절: permission");
    let mut denied = None;
    while let Ok(e) = rx.try_recv() {
        if let Event::Denied(r) = e {
            denied = Some(r);
        }
    }
    assert_eq!(denied.as_deref(), Some("permission"));
    assert_eq!(counts.prompts.load(Ordering::Relaxed), 1, "그 기기 앞에 권한 창을 한 번");
}

fn input(v: Value) -> (u8, Vec<u8>) {
    (ctl::INPUT, v.to_string().into_bytes())
}

#[tokio::test(flavor = "multi_thread")]
async fn input_lands_on_the_captured_display_in_order_and_held_buttons_are_released() {
    let counts = Arc::new(Counts::default());
    let (_router, _viewer, mut rx, ctl, run) = session(FakeCapture { counts: counts.clone(), deny: None, deny_input: None }).await;
    next_frame(&mut rx).await;
    ctl.send(input(json!({ "t": "pointer", "x": 0.25, "y": 0.5, "b": 1 }))).unwrap();
    ctl.send(input(json!({ "t": "pointer", "x": 0.3, "y": 0.5, "b": 1 }))).unwrap();
    ctl.send(input(json!({ "t": "text", "s": "안녕" }))).unwrap();
    ctl.send(input(json!({ "t": "nonsense" }))).unwrap();
    let waited = Instant::now();
    while counts.inputs.lock().unwrap().len() < 3 && waited.elapsed() < T {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    ctl.send((ctl::QUIT, Vec::new())).unwrap();
    tokio::time::timeout(T, run).await.unwrap().unwrap();
    while counts.inputs.lock().unwrap().len() < 4 && waited.elapsed() < T {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let got = counts.inputs.lock().unwrap().clone();
    assert!(got.iter().all(|(d, _)| *d == 7), "세션이 뜨는 화면에 넣는다");
    let kinds: Vec<Input> = got.into_iter().map(|(_, i)| i).collect();
    assert_eq!(kinds[0], Input::Pointer { x: 0.25, y: 0.5, buttons: 1, prev: 0, mods: 0 }, "첫 누름");
    assert_eq!(kinds[1], Input::Pointer { x: 0.3, y: 0.5, buttons: 1, prev: 1, mods: 0 }, "끌기는 직전 단추와 견준다");
    assert_eq!(kinds[2], Input::Text("안녕".into()));
    assert_eq!(kinds[3], Input::Pointer { x: 0.3, y: 0.5, buttons: 0, prev: 1, mods: 0 }, "끊길 때 눌린 단추를 그 자리에서 뗀다");
    assert_eq!(kinds.len(), 4, "모르는 메시지는 버린다");
    let mut told = Vec::new();
    while let Ok(e) = rx.try_recv() {
        if let Event::Input { ok, reason } = e {
            told.push((ok, reason));
        }
    }
    assert_eq!(told, vec![(true, None)], "사정은 바뀔 때만 한 번");
}

#[tokio::test(flavor = "multi_thread")]
async fn input_without_permission_says_so_once_and_asks_once() {
    let counts = Arc::new(Counts::default());
    let (_router, _viewer, mut rx, ctl, run) =
        session(FakeCapture { counts: counts.clone(), deny: None, deny_input: Some(Denied::Permission) }).await;
    next_frame(&mut rx).await;
    for _ in 0..3 {
        ctl.send(input(json!({ "t": "key", "k": "Enter" }))).unwrap();
    }
    let mut told = Vec::new();
    let waited = Instant::now();
    while told.is_empty() && waited.elapsed() < T {
        if let Ok(Some(Event::Input { ok, reason })) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
            told.push((ok, reason));
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    while let Ok(e) = rx.try_recv() {
        if let Event::Input { ok, reason } = e {
            told.push((ok, reason));
        }
    }
    assert_eq!(told, vec![(false, Some("permission".into()))]);
    assert_eq!(counts.input_prompts.load(Ordering::Relaxed), 1, "권한 창은 연결마다 한 번");
    ctl.send((ctl::QUIT, Vec::new())).unwrap();
    tokio::time::timeout(T, run).await.unwrap().unwrap();
}

#[test]
fn frame_round_trips_and_refuses_truncation() {
    let f = Frame {
        seq: 7,
        cap_id: 3,
        key: true,
        t_cap: 1.5,
        t_enc: 1.52,
        width: 3024,
        height: 1964,
        param_sets: vec![vec![0x40, 1], vec![0x42]],
        data: b"nal".to_vec(),
    };
    let buf = f.encode();
    assert_eq!(Frame::decode(&buf), Some(f));
    for cut in 1..40 {
        assert!(Frame::decode(&buf[..cut]).is_none() || cut >= buf.len() - 3, "잘린 머리는 거절: {cut}");
    }
    assert!(Frame::decode(b"Xjunk").is_none());
}

#[test]
fn viewer_config_is_clamped_on_the_sending_side() {
    let c = Config::from_json(&json!({ "codec": "vp9", "bitrate": 1u64 << 40, "fps": 0, "scale": 4.0, "refine": 100000 }));
    assert_eq!(c, Config { codec: "hevc".into(), bitrate: 200_000_000, fps: 1, scale: 1.0, refine: 240 });
    assert_eq!(Config::from_json(&Value::Null), Config::default());
    assert_eq!(Config::from_json(&Config { codec: "h264".into(), ..Config::default() }.to_json()).codec, "h264");
}
