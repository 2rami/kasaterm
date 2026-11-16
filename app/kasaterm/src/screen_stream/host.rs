//! 보내는 쪽 — SCK 로 뜬 장을 VT 로 압축해 `kasa_remote::screen_stream` 에 넘긴다.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kasa_remote::screen_mirror::Denied;
use kasa_remote::screen_stream::{Capture, Config, Frame, Session};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer};
use serde_json::{json, Value};

use super::capture;
use super::codec::{Codec, Encoder, EncoderConfig, FrameMeta};
use super::scroll_probe::Scroller;

pub(crate) struct Host;

struct Pixels(CFRetained<CVPixelBuffer>);
// SCK·VT 버퍼는 IOSurface 라 스레드를 건너도 된다 — 쓰는 쪽은 이 모듈뿐이고 읽기만 한다.
unsafe impl Send for Pixels {}
unsafe impl Sync for Pixels {}

struct Shared {
    encoder: Mutex<Option<Encoder>>,
    force_key: AtomicBool,
    seq: AtomicU32,
    cap_id: AtomicU64,
    /// 마지막으로 그려진 장 — 멈춘 뒤 다듬기와 키프레임 청에 다시 보낸다.
    last: Mutex<Option<(Pixels, u64, f64)>>,
    refine: u32,
    refine_left: AtomicU32,
    last_encode: Mutex<Instant>,
    stop: AtomicBool,
    /// 검증 리그의 움직이는 화면 — 켜져 있으면 SCK 장은 밑그림으로만 쓴다.
    scroll_mode: bool,
    scroll: Mutex<Option<Scroller>>,
    base: Mutex<Option<Pixels>>,
}

impl Shared {
    fn submit(&self, px: &CVPixelBuffer, cap_id: u64, t_cap: f64) {
        let guard = self.encoder.lock().unwrap_or_else(|e| e.into_inner());
        let Some(enc) = guard.as_ref() else { return };
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let force = self.force_key.swap(false, Ordering::Relaxed);
        enc.encode(px, FrameMeta { seq, cap_id, t_cap }, force, super::host_now());
        *self.last_encode.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn resubmit_last(&self) {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(px, id, t)| (Pixels(px.0.clone()), *id, *t));
        if let Some((px, cap_id, t_cap)) = last {
            self.submit(&px.0, cap_id, t_cap);
        }
    }
}

struct HostSession {
    shared: Arc<Shared>,
    capture: Option<capture::Capture>,
    ticker: Option<JoinHandle<()>>,
    info: Value,
}

impl Session for HostSession {
    fn keyframe(&self) {
        self.shared.force_key.store(true, Ordering::Relaxed);
        // 멈춘 화면이면 SCK 가 새 장을 안 줘 키프레임이 안 나간다 — 마지막 장을 한 번 더 보낸다.
        self.shared.refine_left.fetch_max(1, Ordering::Relaxed);
    }

    fn info(&self) -> Value {
        self.info.clone()
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
        }
        drop(self.capture.take());
        *self.shared.encoder.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

fn keep_awake() {
    if let Some(source) = kasa_remote::screen_mirror::installed() {
        source.keep_awake();
    }
}

fn permitted() -> bool {
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    unsafe { CGPreflightScreenCaptureAccess() }
}

impl Capture for Host {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        // 권한 없이도 SCK 는 배경화면뿐인 거짓 화면을 줄 수 있다 — 먼저 가른다.
        if !permitted() {
            return Err(Denied::Permission);
        }
        keep_awake();
        let shared = Arc::new(Shared {
            encoder: Mutex::new(None),
            force_key: AtomicBool::new(true),
            seq: AtomicU32::new(0),
            cap_id: AtomicU64::new(0),
            last: Mutex::new(None),
            refine: config.refine,
            // 첫 장은 화면이 멈춰 있어도 한 번은 보낸다.
            refine_left: AtomicU32::new(config.refine.max(1)),
            last_encode: Mutex::new(Instant::now()),
            stop: AtomicBool::new(false),
            scroll_mode: Scroller::wanted(),
            scroll: Mutex::new(None),
            base: Mutex::new(None),
        });
        let sink = shared.clone();
        let cap = capture::Capture::start(
            config.fps,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            f64::from(config.scale),
            move |f| {
                if sink.scroll_mode {
                    *sink.base.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pixels(f.pixels));
                    return;
                }
                let cap_id = sink.cap_id.fetch_add(1, Ordering::Relaxed) + 1;
                sink.submit(&f.pixels, cap_id, f.t_cap);
                *sink.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Pixels(f.pixels), cap_id, f.t_cap));
                sink.refine_left.store(sink.refine, Ordering::Relaxed);
            },
        )
        .map_err(|e| {
            eprintln!("[screen-stream] 화면 뜨기를 못 열었다: {e:#}");
            Denied::Unavailable
        })?;
        let (width, height) = (cap.width as u32, cap.height as u32);
        if shared.scroll_mode {
            *shared.scroll.lock().unwrap_or_else(|e| e.into_inner()) = Scroller::from_env(cap.width, cap.height);
        }
        let enc_cfg = EncoderConfig { codec: Codec::named(&config.codec), width: cap.width, height: cap.height, bitrate: i64::from(config.bitrate), fps: config.fps };
        let encoder = Encoder::new(&enc_cfg, move |e| {
            out(Frame {
                seq: e.seq,
                cap_id: e.cap_id,
                key: e.key,
                t_cap: e.t_cap,
                t_enc: e.t_enc,
                width,
                height,
                param_sets: e.param_sets,
                data: e.data,
            })
        })
        .map_err(|e| {
            eprintln!("[screen-stream] 압축기를 못 열었다: {e:#}");
            Denied::Unavailable
        })?;
        let info = json!({
            "width": width, "height": height, "codec": config.codec, "fps": config.fps,
            "bitrate": config.bitrate, "refine": config.refine, "rejected": encoder.rejected,
        });
        *shared.encoder.lock().unwrap_or_else(|e| e.into_inner()) = Some(encoder);
        let ticker = std::thread::Builder::new()
            .name("screen-stream-refine".into())
            .spawn({
                let shared = shared.clone();
                let fps = config.fps.max(1);
                move || refine_loop(&shared, fps)
            })
            .ok();
        Ok(Box::new(HostSession { shared, capture: Some(cap), ticker, info }))
    }

    fn request_permission(&self) {
        if let Some(source) = kasa_remote::screen_mirror::installed() {
            source.request_permission();
        }
    }

    fn now(&self) -> f64 {
        super::host_now()
    }
}

/// 화면이 멈춰 SCK 가 장을 안 줄 때 마지막 장을 `refine` 번 더 압축한다. 율 제어가 남는 비트로 품질을 올려 움직일 때
/// 무른 장이 0.5초 안에 멈춘 화면 기준선(PSNR 52dB)까지 돌아온다(S0). 계속 보내면 새 장이 앞 장 압축을 기다려
/// 지연이 늘어 거기서 멈춘다. 보는 사람이 있는 동안 화면이 잠들지 않게도 한다.
fn refine_loop(shared: &Shared, fps: u32) {
    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let mut awake_at = Instant::now();
    let mut due = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        if shared.scroll_mode {
            due += period;
            match due.checked_duration_since(Instant::now()) {
                Some(wait) => std::thread::sleep(wait),
                None => due = Instant::now(),
            }
            let base = shared.base.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|b| Pixels(b.0.clone()));
            let next = base.and_then(|b| shared.scroll.lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.next(&b.0));
            if let Some(px) = next {
                let cap_id = shared.cap_id.fetch_add(1, Ordering::Relaxed) + 1;
                shared.submit(&px, cap_id, super::host_now());
            }
            continue;
        }
        std::thread::sleep(period / 2);
        if awake_at.elapsed() >= Duration::from_secs(20) {
            keep_awake();
            awake_at = Instant::now();
        }
        let idle = shared.last_encode.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= period;
        if idle && shared.refine_left.load(Ordering::Relaxed) > 0 {
            shared.refine_left.fetch_sub(1, Ordering::Relaxed);
            shared.resubmit_last();
        }
    }
}
