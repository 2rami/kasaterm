//! 화면 칸 소리 — 그 기기 시스템 소리를 PCM 덩이로 카사넷 QUIC 데이터그램에 싣는다. 계약·실측은
//! `docs/screen-stream.md` 「S4 소리」.
//!
//! - 덩이 `'A'` + seq(u32) + t_cap(f64, 보내는 쪽 호스트 시계) + 장 수(u16) + s16le 48kHz 스테레오. 한 덩이가 5ms(960바이트)다 —
//!   데이터그램은 꾸러미 하나(경로 MTU, 적어도 1KB 남짓)에 들어야 해서 10ms(1920바이트)는 못 싣는다.
//! - 데이터그램이라 영상 장(장마다 스트림) 뒤에 줄 서지 않는다 — 꾸러미를 채울 때 데이터그램이 스트림보다 먼저고, 보는 쪽도
//!   장 스트림을 받는 차례와 따로 읽는다. 잃거나 늦은 덩이는 다시 보내지 않는다.
//! - 압축 없이 PCM 이다(약 1.6Mbps, 영상 30Mbps 곁에서 작다). 코덱 의존성을 들이지 않는다.
//! - 뜨기는 기기에 하나(`Hub`)다. 소리를 받겠다는 연결이 구독하고, 마지막 구독이 떠나면 멈춘다.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use bytes::Bytes;

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// 5ms — 데이터그램 하나에 드는 가장 큰 덩이(위 모듈 설명).
pub const CHUNK_FRAMES: usize = 240;
const HEAD: usize = 1 + 4 + 8 + 2;
/// 덩이 하나의 장 수 상한 — 받은 값을 그대로 믿지 않는다.
const FRAMES_MAX: usize = 4800;

/// 보내는 쪽 시계가 끊겼다 다시 이어질 때(소리가 멎었다 다시 남) 새로 셀 틈. WASAPI 루프백은 소리가 없으면 아무것도 안 준다.
const GAP_S: f64 = 0.01;

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub seq: u32,
    /// 첫 장이 소리로 난 시각(보내는 쪽 호스트 시계 초).
    pub t_cap: f64,
    /// 왼쪽·오른쪽이 번갈아 든 s16 장들.
    pub pcm: Vec<i16>,
}

impl Chunk {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEAD + self.pcm.len() * 2);
        out.push(b'A');
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.t_cap.to_be_bytes());
        out.extend_from_slice(&(self.frames() as u16).to_be_bytes());
        for s in &self.pcm {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < HEAD || buf[0] != b'A' {
            return None;
        }
        let seq = u32::from_be_bytes(buf[1..5].try_into().ok()?);
        let t_cap = f64::from_be_bytes(buf[5..13].try_into().ok()?);
        let frames = u16::from_be_bytes(buf[13..15].try_into().ok()?) as usize;
        let body = &buf[HEAD..];
        if frames == 0 || frames > FRAMES_MAX || body.len() != frames * CHANNELS * 2 || !t_cap.is_finite() {
            return None;
        }
        let pcm = body.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        Some(Self { seq, t_cap, pcm })
    }

    pub fn frames(&self) -> usize {
        self.pcm.len() / CHANNELS
    }

    fn peak(&self) -> u16 {
        self.pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0)
    }
}

// ---------------------------------------------------------------- 보내는 쪽

/// 앱이 꽂는 시스템 소리 뜨기. `out` 은 (첫 장의 호스트 시계 초, 48kHz 스테레오 s16 장들)이고 뜨기 스레드에서 부른다.
/// 돌려준 것을 버리면 멈춘다(멈추기를 기다려도 된다 — 허브 스레드가 부른다).
pub trait Source: Send + Sync {
    fn start(&self, out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String>;
}

type Sink = Arc<dyn Fn(Bytes) + Send + Sync>;

#[derive(Default)]
struct HubState {
    subs: HashMap<u64, Sink>,
    next_id: u64,
    /// 구독이 바뀐 횟수 — 뜨기를 못 연 뒤에는 구독이 바뀔 때까지 다시 열지 않는다.
    generation: u64,
    thread: bool,
}

/// 기기에 하나인 소리 뜨기. 구독이 있으면 뜨고 없으면 멈춘다 — 여닫기는 허브 스레드가 해서 구독·해지는 막히지 않는다
/// (SCK 시작·멈춤은 끝 알림을 기다린다).
pub struct Hub {
    source: Arc<dyn Source>,
    state: Mutex<HubState>,
    changed: Condvar,
}

/// seq 는 뜨기를 다시 열어도 이어 센다 — 소리를 껐다 켠 보는 쪽이 새 덩이를 「이미 지난 덩이」로 버리지 않게.
static SEQ: AtomicU32 = AtomicU32::new(0);

impl Hub {
    pub fn new(source: Arc<dyn Source>) -> Arc<Self> {
        Arc::new(Self { source, state: Mutex::new(HubState::default()), changed: Condvar::new() })
    }

    /// 덩이마다 `send` 를 뜨기 스레드에서 부른다. 돌려준 것을 버리면 해지한다.
    pub fn subscribe(self: &Arc<Self>, send: impl Fn(Bytes) + Send + Sync + 'static) -> Subscription {
        let mut st = self.lock();
        let id = st.next_id;
        st.next_id += 1;
        st.generation += 1;
        st.subs.insert(id, Arc::new(send));
        if !std::mem::replace(&mut st.thread, true) {
            let hub = self.clone();
            if std::thread::Builder::new().name("screen-audio-hub".into()).spawn(move || hub.run()).is_err() {
                st.thread = false;
            }
        }
        drop(st);
        self.changed.notify_all();
        Subscription { hub: self.clone(), id }
    }

    pub fn subscribers(&self) -> usize {
        self.lock().subs.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn run(self: Arc<Self>) {
        let mut running: Option<Box<dyn Send>> = None;
        let mut failed_at: Option<u64> = None;
        loop {
            let mut st = self.lock();
            let generation = loop {
                let want = !st.subs.is_empty();
                if (want && running.is_none() && failed_at != Some(st.generation)) || (!want && running.is_some()) {
                    break st.generation;
                }
                st = self.changed.wait(st).unwrap_or_else(|e| e.into_inner());
            };
            let want = !st.subs.is_empty();
            drop(st);
            if !want {
                drop(running.take());
                eprintln!("[screen-audio] 시스템 소리 뜨기 멈춤");
                continue;
            }
            let hub = Arc::downgrade(&self);
            let mut chunker = Chunker::default();
            match self.source.start(Box::new(move |t, pcm| chunker.feed(t, pcm, |t, pcm| emit(&hub, t, pcm)))) {
                Ok(guard) => {
                    running = Some(guard);
                    eprintln!("[screen-audio] 시스템 소리 뜨기 시작");
                }
                Err(e) => {
                    eprintln!("[screen-audio] 시스템 소리를 못 뜬다: {e}");
                    failed_at = Some(generation);
                }
            }
        }
    }
}

fn emit(hub: &Weak<Hub>, t_cap: f64, pcm: &[i16]) {
    let Some(hub) = hub.upgrade() else { return };
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let bytes = Bytes::from(Chunk { seq, t_cap, pcm: pcm.to_vec() }.encode());
    let subs: Vec<Sink> = hub.lock().subs.values().cloned().collect();
    for send in subs {
        send(bytes.clone());
    }
}

pub struct Subscription {
    hub: Arc<Hub>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut st = self.hub.lock();
        st.subs.remove(&self.id);
        st.generation += 1;
        drop(st);
        self.hub.changed.notify_all();
    }
}

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

/// 앱이 부팅 때 한 번 — 그 뒤 붙는 화면 연결이 소리를 실을 수 있다.
pub fn install(source: Arc<dyn Source>) {
    let _ = HUB.set(Hub::new(source));
}

pub fn hub() -> Option<Arc<Hub>> {
    HUB.get().cloned()
}

/// 뜨기가 내는 크기 제각각의 덩이(SCK 는 약 21ms, WASAPI 는 장치 주기)를 5ms 덩이로 자른다.
#[derive(Default)]
struct Chunker {
    buf: Vec<i16>,
    t0: f64,
}

impl Chunker {
    fn feed(&mut self, t: f64, pcm: &[i16], mut emit: impl FnMut(f64, &[i16])) {
        let expected = self.t0 + (self.buf.len() / CHANNELS) as f64 / f64::from(RATE);
        if !self.buf.is_empty() && (t - expected).abs() > GAP_S {
            self.buf.clear();
        }
        if self.buf.is_empty() {
            self.t0 = t;
        }
        self.buf.extend_from_slice(&pcm[..pcm.len() - pcm.len() % CHANNELS]);
        let n = CHUNK_FRAMES * CHANNELS;
        let mut at = 0;
        while self.buf.len() - at >= n {
            emit(self.t0, &self.buf[at..at + n]);
            at += n;
            self.t0 += CHUNK_FRAMES as f64 / f64::from(RATE);
        }
        self.buf.drain(..at);
    }
}

// ---------------------------------------------------------------- 보는 쪽

/// 틀기 시작할 때·끊겼다 다시 틀 때 먼저 모아 둘 만큼.
const START_MS: f64 = 40.0;
/// 1초 동안 꺼내고 남은 가장 적은 양이 이보다 많으면 `KEEP_MS` 까지 버린다 — 한 번 밀린 지연·두 기기 소리 시계의 어긋남이
/// 쌓이지 않게.
const TRIM_ABOVE_MS: f64 = 30.0;
const KEEP_MS: f64 = 15.0;
/// 한꺼번에 몰려 이만큼 넘게 쌓이면 바로 `START_MS` 까지 버린다.
const MAX_MS: f64 = 150.0;
/// 받았을 때 이보다 오래된 덩이는 버린다(보내는 쪽 시계로 잰 나이).
pub const LATE_S: f64 = 0.25;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JitterStats {
    pub received: u64,
    /// 늦어 버린 덩이 — 너무 오래됐거나 그 자리를 이미 지나 틀었다.
    pub late: u64,
    /// 따라잡느라 버린 덩이.
    pub trimmed: u64,
    /// 오지 않아 건너뛴 덩이.
    pub lost: u64,
    pub underruns: u64,
}

/// 받은 덩이를 seq 차례로 모았다가 틀기에 내준다. 먼저 `START_MS` 를 모으고, 비면 소리 없음으로 채운 뒤 다시 모은다.
#[derive(Default)]
pub struct Jitter {
    chunks: BTreeMap<u32, Vec<i16>>,
    cur: Vec<i16>,
    cur_at: usize,
    next: Option<u32>,
    playing: bool,
    window_frames: usize,
    window_min: Option<usize>,
    pub stats: JitterStats,
    /// 지난번 `take_peak` 뒤 받은 가장 큰 진폭.
    peak: u16,
}

fn frames_of(ms: f64) -> usize {
    (ms * f64::from(RATE) / 1000.0) as usize
}

impl Jitter {
    /// `age` 는 받은 때 그 덩이의 나이(초, 보내는 쪽 시계로).
    pub fn push(&mut self, chunk: Chunk, age: f64) {
        self.stats.received += 1;
        self.peak = self.peak.max(chunk.peak());
        let behind = self.next.is_some_and(|n| (chunk.seq.wrapping_sub(n) as i32) < 0);
        if age > LATE_S || behind || self.chunks.contains_key(&chunk.seq) {
            self.stats.late += 1;
            return;
        }
        self.chunks.insert(chunk.seq, chunk.pcm);
        if self.buffered_frames() > frames_of(MAX_MS) {
            self.trim_to(frames_of(START_MS));
        }
    }

    /// 틀 장들(왼쪽·오른쪽 번갈아)을 채운다. 모으는 중이거나 비면 소리 없음. 소리를 하나라도 냈으면 참.
    pub fn pull(&mut self, out: &mut [i16]) -> bool {
        if !self.playing {
            if self.buffered_frames() < frames_of(START_MS) {
                out.fill(0);
                return false;
            }
            self.playing = true;
            self.window_frames = 0;
            self.window_min = None;
        }
        let mut at = 0;
        while at < out.len() {
            if self.cur_at >= self.cur.len() {
                let Some((seq, pcm)) = self.chunks.pop_first() else {
                    self.stats.underruns += 1;
                    self.playing = false;
                    out[at..].fill(0);
                    break;
                };
                if let Some(n) = self.next {
                    self.stats.lost += u64::from(seq.wrapping_sub(n));
                }
                self.next = Some(seq.wrapping_add(1));
                self.cur = pcm;
                self.cur_at = 0;
            }
            let n = (self.cur.len() - self.cur_at).min(out.len() - at);
            out[at..at + n].copy_from_slice(&self.cur[self.cur_at..self.cur_at + n]);
            at += n;
            self.cur_at += n;
        }
        if self.playing {
            let left = self.buffered_frames();
            self.window_min = Some(self.window_min.map_or(left, |m| m.min(left)));
            self.window_frames += out.len() / CHANNELS;
            if self.window_frames >= RATE as usize {
                if let Some(min) = self.window_min.filter(|m| *m > frames_of(TRIM_ABOVE_MS)) {
                    let target = left.saturating_sub(min - frames_of(KEEP_MS));
                    self.trim_to(target);
                }
                self.window_frames = 0;
                self.window_min = None;
            }
        }
        at > 0
    }

    /// 모아 둔 양(ms).
    pub fn buffered_ms(&self) -> f64 {
        self.buffered_frames() as f64 * 1000.0 / f64::from(RATE)
    }

    pub fn take_peak(&mut self) -> u16 {
        std::mem::take(&mut self.peak)
    }

    pub fn reset(&mut self) {
        let stats = self.stats;
        *self = Self { stats, ..Self::default() };
    }

    fn buffered_frames(&self) -> usize {
        (self.chunks.values().map(Vec::len).sum::<usize>() + self.cur.len() - self.cur_at) / CHANNELS
    }

    /// 오래된 덩이부터 버려 `keep` 장 남짓만 남긴다. 틀던 덩이는 끝까지 튼다(덩이 한가운데서 끊으면 더 크게 튄다).
    fn trim_to(&mut self, keep: usize) {
        while self.buffered_frames() > keep {
            let Some((seq, _)) = self.chunks.pop_first() else { break };
            self.next = Some(seq.wrapping_add(1));
            self.stats.trimmed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u32, value: i16) -> Chunk {
        Chunk { seq, t_cap: f64::from(seq) * 0.005, pcm: vec![value; CHUNK_FRAMES * CHANNELS] }
    }

    fn pull(j: &mut Jitter, frames: usize) -> Vec<i16> {
        let mut out = vec![1; frames * CHANNELS];
        j.pull(&mut out);
        out
    }

    #[test]
    fn a_chunk_survives_the_wire_and_bad_ones_are_refused() {
        let c = Chunk { seq: 7, t_cap: 12.5, pcm: (0..CHUNK_FRAMES * CHANNELS).map(|i| i as i16 - 240).collect() };
        let bytes = c.encode();
        assert_eq!(bytes.len(), HEAD + 960, "5ms 덩이는 데이터그램 하나(1KB 남짓)에 든다");
        assert_eq!(Chunk::decode(&bytes), Some(c));
        assert_eq!(Chunk::decode(&bytes[..bytes.len() - 1]), None, "장 수와 길이가 어긋나면 버린다");
        let mut wrong = bytes.clone();
        wrong[0] = b'F';
        assert_eq!(Chunk::decode(&wrong), None);
        assert_eq!(Chunk::decode(&[b'A', 0, 0]), None);
    }

    #[test]
    fn the_chunker_cuts_capture_buffers_into_5ms_chunks_with_their_own_times() {
        let mut c = Chunker::default();
        let mut got: Vec<(f64, usize)> = Vec::new();
        // SCK 는 1024장(21.3ms)씩 준다.
        let buf = vec![3i16; 1024 * CHANNELS];
        c.feed(10.0, &buf, |t, pcm| got.push((t, pcm.len())));
        c.feed(10.0 + 1024.0 / 48_000.0, &buf, |t, pcm| got.push((t, pcm.len())));
        assert_eq!(got.len(), 2048 / CHUNK_FRAMES);
        assert!(got.iter().all(|(_, n)| *n == CHUNK_FRAMES * CHANNELS));
        for (i, (t, _)) in got.iter().enumerate() {
            assert!((t - (10.0 + i as f64 * 0.005)).abs() < 1e-9, "덩이 시각은 이어 센다: {i} {t}");
        }
        got.clear();
        // 소리가 멎었다 1초 뒤 다시 나면 남은 조각을 버리고 새 시각으로 센다.
        c.feed(12.0, &vec![1i16; CHUNK_FRAMES * CHANNELS], |t, pcm| got.push((t, pcm.len())));
        assert_eq!(got, vec![(12.0, CHUNK_FRAMES * CHANNELS)]);
    }

    #[test]
    fn playback_waits_for_the_start_level_then_plays_in_order() {
        let mut j = Jitter::default();
        for s in 0..7 {
            j.push(chunk(s, s as i16 + 1), 0.01);
        }
        assert!(pull(&mut j, 240).iter().all(|s| *s == 0), "35ms 로는 아직 안 튼다");
        j.push(chunk(7, 8), 0.01);
        assert!((j.buffered_ms() - 40.0).abs() < 0.01);
        let out = pull(&mut j, 480);
        assert!(out[..480].iter().all(|s| *s == 1) && out[480..].iter().all(|s| *s == 2), "seq 차례로 이어 튼다");
    }

    #[test]
    fn late_and_old_chunks_are_dropped() {
        let mut j = Jitter::default();
        for s in 10..18 {
            j.push(chunk(s, 1), 0.01);
        }
        pull(&mut j, 480);
        j.push(chunk(10, 9), 0.01);
        j.push(chunk(30, 9), LATE_S + 0.01);
        assert_eq!(j.stats.late, 2, "이미 지나 틀었거나 너무 오래된 덩이");
        assert_eq!(j.stats.received, 10);
    }

    #[test]
    fn a_missing_chunk_is_skipped_and_counted() {
        let mut j = Jitter::default();
        for s in (0..10).filter(|s| *s != 3) {
            j.push(chunk(s, s as i16), 0.01);
        }
        let out = pull(&mut j, 240 * 4);
        assert_eq!(out[240 * 2 * 3], 4, "3 을 건너뛰고 4 를 튼다");
        assert_eq!(j.stats.lost, 1);
        j.push(chunk(3, 3), 0.01);
        assert_eq!(j.stats.late, 1, "건너뛴 뒤 온 덩이는 늦은 것");
    }

    #[test]
    fn running_dry_plays_silence_and_refills_before_playing_again() {
        let mut j = Jitter::default();
        for s in 0..8 {
            j.push(chunk(s, 5), 0.01);
        }
        let out = pull(&mut j, 240 * 10);
        assert!(out[..240 * 2 * 8].iter().all(|s| *s == 5) && out[240 * 2 * 8..].iter().all(|s| *s == 0));
        assert_eq!(j.stats.underruns, 1);
        j.push(chunk(8, 6), 0.01);
        assert!(pull(&mut j, 240).iter().all(|s| *s == 0), "다시 40ms 를 모을 때까지 기다린다");
    }

    #[test]
    fn a_burst_after_a_stall_is_cut_back_to_the_start_level() {
        let mut j = Jitter::default();
        for s in 0..60 {
            j.push(chunk(s, s as i16), 0.01);
        }
        assert!(j.buffered_ms() <= MAX_MS, "몰려온 300ms 를 다 들고 있지 않는다: {}", j.buffered_ms());
        assert!(j.stats.trimmed >= 40, "{:?}", j.stats);
        let out = pull(&mut j, 240);
        assert!(out[0] >= 40, "오래된 덩이를 버리고 새 덩이부터 튼다: {}", out[0]);
    }

    #[test]
    fn a_faster_sender_clock_does_not_pile_up_latency() {
        // 보내는 쪽이 1% 빠르다 — 60초 동안 10ms 씩 꺼내면 버퍼가 600ms 늘 일이다.
        let mut j = Jitter::default();
        let (mut seq, mut sent) = (0u32, 0.0f64);
        let mut worst: f64 = 0.0;
        for tick in 0..6000 {
            let due = (f64::from(tick) + 1.0) * 480.0 * 1.01;
            while sent + CHUNK_FRAMES as f64 <= due {
                j.push(chunk(seq, 1), 0.01);
                seq += 1;
                sent += CHUNK_FRAMES as f64;
            }
            pull(&mut j, 480);
            if tick > 300 {
                worst = worst.max(j.buffered_ms());
            }
        }
        assert!(worst < START_MS + 20.0, "지연이 쌓이지 않는다: 가장 많이 {worst}ms");
        assert_eq!(j.stats.underruns, 0, "줄이느라 비지 않는다");
        assert!(j.stats.trimmed > 0);
    }

    struct Fake {
        started: Arc<std::sync::atomic::AtomicUsize>,
        stopped: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct Running(Arc<std::sync::atomic::AtomicBool>, Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for Running {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            self.1.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Source for Fake {
        fn start(&self, mut out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String> {
            self.started.fetch_add(1, Ordering::Relaxed);
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let s = stop.clone();
            std::thread::spawn(move || {
                let mut t = 0.0;
                while !s.load(Ordering::Relaxed) {
                    out(t, &[1; CHUNK_FRAMES * CHANNELS]);
                    t += 0.005;
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            });
            Ok(Box::new(Running(stop, self.stopped.clone())))
        }
    }

    fn wait(what: impl Fn() -> bool) {
        let at = std::time::Instant::now();
        while !what() {
            assert!(at.elapsed().as_secs() < 10, "기다리다 지쳤다");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn the_hub_captures_once_for_all_subscribers_and_stops_after_the_last_leaves() {
        use std::sync::atomic::AtomicUsize;
        let (started, stopped) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let hub = Hub::new(Arc::new(Fake { started: started.clone(), stopped: stopped.clone() }));
        let (a_got, b_got) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let a = {
            let n = a_got.clone();
            hub.subscribe(move |b| {
                assert!(Chunk::decode(&b).is_some());
                n.fetch_add(1, Ordering::Relaxed);
            })
        };
        let b = {
            let n = b_got.clone();
            hub.subscribe(move |_| {
                n.fetch_add(1, Ordering::Relaxed);
            })
        };
        wait(|| a_got.load(Ordering::Relaxed) > 3 && b_got.load(Ordering::Relaxed) > 3);
        assert_eq!(started.load(Ordering::Relaxed), 1, "보는 쪽이 둘이어도 뜨기는 하나");
        drop(a);
        let a_then = a_got.load(Ordering::Relaxed);
        let b_then = b_got.load(Ordering::Relaxed);
        wait(|| b_got.load(Ordering::Relaxed) > b_then + 3);
        assert!(a_got.load(Ordering::Relaxed) <= a_then + 1, "해지한 쪽에는 그만 보낸다");
        assert_eq!(stopped.load(Ordering::Relaxed), 0, "남은 쪽이 있으면 계속 뜬다");
        drop(b);
        wait(|| stopped.load(Ordering::Relaxed) == 1);
        assert_eq!(hub.subscribers(), 0);
        let _c = hub.subscribe(|_| {});
        wait(|| started.load(Ordering::Relaxed) == 2);
    }
}
