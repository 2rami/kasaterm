//! 보는 쪽 — 카사넷으로 받은 장을 풀기 일꾼이 순서대로 VT 로 풀고, 마지막 장만 GUI 가 가져가 그린다.
//! 칸 하나에 `ScreenView` 하나. 버리면 연결과 일꾼이 함께 닫힌다.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use kasa_remote::screen_stream::{self, Config, Event, Frame, Keyframes, Watch};
use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBuffer;

use super::codec::{self, Codec, Decoder};

struct Pixels(CFRetained<CVPixelBuffer>);
// VT 가 내준 IOSurface 버퍼 — 일꾼이 넣고 GUI 가 읽기만 한다.
unsafe impl Send for Pixels {}

struct Incoming {
    frame: Frame,
    offset: f64,
}

#[derive(Default)]
struct Stats {
    /// 최근 1초의 (풀린 시각, 화면→풀림 ms, 바이트).
    window: VecDeque<(f64, f64, usize)>,
    path: &'static str,
    frames: u64,
    latency_sum: f64,
}

struct Shared {
    latest: Mutex<Option<Pixels>>,
    serial: AtomicU64,
    status: Mutex<String>,
    stats: Mutex<Stats>,
}

pub(crate) struct ScreenView {
    pub label: String,
    shared: Arc<Shared>,
    watch: Option<Watch>,
    uploaded: u64,
    /// 마지막 장을 GPU 로 올리는 데 든 시간(ms)과 올린 장 수 — 검증 리그가 읽는다.
    pub upload_ms: f64,
    pub uploads: u64,
}

impl ScreenView {
    /// 그 기기(base) 화면을 본다. 새 장이 풀릴 때마다 `wake` 를 부른다 — GUI 를 깨워 다시 그리게.
    pub(crate) fn open(label: &str, base: &str, wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            serial: AtomicU64::new(0),
            status: Mutex::new("카사넷으로 붙는 중".into()),
            stats: Mutex::new(Stats::default()),
        });
        let config = Config::default();
        let (tx, rx) = mpsc::channel::<Incoming>();
        let keyframes: Arc<Mutex<Option<Keyframes>>> = Arc::new(Mutex::new(None));
        let on = {
            let (shared, wake) = (shared.clone(), wake.clone());
            let tx = Mutex::new(tx);
            move |event: Event| match event {
                Event::Connected { path, .. } => {
                    shared.stats.lock().unwrap_or_else(|e| e.into_inner()).path = path;
                    set_status(&shared, &wake, "화면을 받는 중");
                }
                Event::Info(_) => {}
                Event::Frame { frame, offset, .. } => {
                    let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(Incoming { frame, offset });
                }
                Event::Denied(reason) => {
                    let text = if reason == "permission" {
                        "그 기기 카사텀에 화면 기록 권한이 없어요 — 그 기기에서 켜고 카사텀을 다시 열어 주세요"
                    } else {
                        "그 기기가 화면을 보낼 수 없어요"
                    };
                    set_status(&shared, &wake, text);
                }
                Event::Closed(why) => set_status(&shared, &wake, &format!("끊겼어요 — {why}")),
            }
        };
        let watch = screen_stream::watch(base.to_string(), config.clone(), on)?;
        *keyframes.lock().unwrap_or_else(|e| e.into_inner()) = Some(watch.keyframes());
        let worker_shared = shared.clone();
        let codec = Codec::named(&config.codec);
        std::thread::Builder::new()
            .name("screen-stream-decode".into())
            .spawn(move || decode_loop(codec, rx, keyframes, &worker_shared, &wake))
            .ok()?;
        Some(Self { label: label.to_string(), shared, watch: Some(watch), uploaded: 0, upload_ms: 0.0, uploads: 0 })
    }

    /// 지난번에 가져간 뒤 새로 풀린 장이 있으면 BGRA 바이트(가로·세로·줄 간격)로 넘긴다.
    pub(crate) fn take_new(&mut self, f: impl FnOnce(&[u8], u32, u32, u32)) {
        let serial = self.shared.serial.load(Ordering::Acquire);
        if serial == self.uploaded {
            return;
        }
        let started = std::time::Instant::now();
        let latest = self.shared.latest.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(px) = latest.as_ref() {
            codec::with_bgra(&px.0, f);
        }
        drop(latest);
        self.upload_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.uploads += 1;
        self.uploaded = serial;
    }

    pub(crate) fn has_new_frame(&self) -> bool {
        self.shared.serial.load(Ordering::Acquire) != self.uploaded
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.uploaded != 0
    }

    pub(crate) fn status(&self) -> String {
        self.shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 칸 위 작은 표시 — `60fps · 24ms · 직통`.
    pub(crate) fn hud(&self) -> String {
        let s = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
        let mut lat: Vec<f64> = s.window.iter().map(|w| w.1).collect();
        lat.sort_by(|a, b| a.total_cmp(b));
        let p50 = lat.get(lat.len() / 2).copied().unwrap_or(0.0);
        let path = if s.path == "kasanet" { "직통" } else { "중계" };
        format!("{}fps · {:.0}ms · {path}", s.window.len(), p50)
    }
}

impl Drop for ScreenView {
    fn drop(&mut self) {
        drop(self.watch.take());
        let s = self.shared.stats.lock().unwrap_or_else(|e| e.into_inner());
        if s.frames > 0 {
            eprintln!(
                "[screen-stream] {} 닫음 — 새 장 {}개, 화면→풀림 평균 {:.1}ms",
                self.label,
                s.frames,
                s.latency_sum / s.frames as f64
            );
        }
    }
}

fn set_status(shared: &Shared, wake: &Arc<dyn Fn() + Send + Sync>, text: &str) {
    *shared.status.lock().unwrap_or_else(|e| e.into_inner()) = text.to_string();
    wake();
}

/// 장은 앞 장을 참조해 순서대로만 풀린다. 단방향 스트림마다 도착 순서가 조금 엇갈려 seq 로 다시 줄 세우고, 키프레임
/// 전의 장은 버린다. 풀기에 실패하면 키프레임을 청하고 다음 키프레임까지 기다린다.
fn decode_loop(
    codec: Codec,
    rx: mpsc::Receiver<Incoming>,
    keyframes: Arc<Mutex<Option<Keyframes>>>,
    shared: &Shared,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    let mut dec = Decoder::new(codec);
    let mut pending: BTreeMap<u32, Incoming> = BTreeMap::new();
    let mut next: Option<u32> = None;
    let mut last_cap: Option<u64> = None;
    let mut asked_key = false;
    let ask_key = || {
        if let Some(k) = keyframes.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            k.request();
        }
    };
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(f) => {
                pending.insert(f.frame.seq, f);
                while let Ok(f) = rx.try_recv() {
                    pending.insert(f.frame.seq, f);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if next.is_none() {
            match pending.iter().find(|(_, f)| f.frame.key).map(|(s, _)| *s) {
                Some(s) => {
                    next = Some(s);
                    pending.retain(|k, _| *k >= s);
                }
                None => {
                    if !asked_key {
                        ask_key();
                        asked_key = true;
                    }
                    pending.clear();
                    continue;
                }
            }
        }
        let mut decoded = false;
        while let Some(seq) = next {
            let Some(mut f) = pending.remove(&seq) else { break };
            next = Some(seq.wrapping_add(1));
            if let Err(e) = dec.decode(&f.frame.param_sets, &mut f.frame.data, f.frame.t_enc) {
                eprintln!("[screen-stream] 풀기 실패 seq={seq}: {e:#} — 키프레임을 청한다");
                next = None;
                asked_key = true;
                ask_key();
                break;
            }
            asked_key = false;
            decoded = true;
            let t_done = screen_stream::now();
            if last_cap != Some(f.frame.cap_id) {
                last_cap = Some(f.frame.cap_id);
                let latency = ((t_done + f.offset) - f.frame.t_cap) * 1000.0;
                let mut s = shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                s.window.push_back((t_done, latency, f.frame.data.len()));
                s.frames += 1;
                s.latency_sum += latency;
            }
        }
        if decoded {
            if let Some(px) = dec.last() {
                *shared.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pixels(px));
                shared.serial.fetch_add(1, Ordering::Release);
                let now = screen_stream::now();
                let mut s = shared.stats.lock().unwrap_or_else(|e| e.into_inner());
                while s.window.front().is_some_and(|w| now - w.0 > 1.0) {
                    s.window.pop_front();
                }
                drop(s);
                wake();
            }
        }
    }
}
