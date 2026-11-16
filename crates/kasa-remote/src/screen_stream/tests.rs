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
}

/// 5ms 마다 장을 낸다. 첫 장·청한 다음 장이 키프레임이다.
struct FakeCapture {
    counts: Arc<Counts>,
    deny: Option<Denied>,
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
async fn session(capture: FakeCapture) -> (Router, Endpoint, mpsc::UnboundedReceiver<Event>, mpsc::UnboundedSender<u8>, tokio::task::JoinHandle<String>) {
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
    let (_router, _viewer, mut rx, ctl, run) = session(FakeCapture { counts: counts.clone(), deny: None }).await;
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
    ctl.send(ctl::KEYFRAME).unwrap();
    let key = loop {
        let (f, _) = next_frame(&mut rx).await;
        if f.key {
            break f;
        }
    };
    assert!(!key.param_sets.is_empty());
    assert_eq!(counts.keyframes.load(Ordering::Relaxed), 1);
    ctl.send(ctl::QUIT).unwrap();
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
    let (_router, _viewer, mut rx, _ctl, run) = session(FakeCapture { counts: counts.clone(), deny: Some(Denied::Permission) }).await;
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
