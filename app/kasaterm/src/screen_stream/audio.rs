//! 화면 칸 소리 — 그 기기 시스템 소리를 칸에서 튼다(`docs/screen-stream.md` 「S4 소리」). 틀·허브·지터 버퍼는
//! `kasa_remote::screen_stream::audio`, 여기는 칸의 「소리」 알약과 OS 의 뜨기·틀기(`audio_mac.rs`·`audio_win.rs`)를 잇는다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kasa_remote::screen_stream::audio::{Chunk, Jitter, CHANNELS, CHUNK_FRAMES, RATE};
use kasa_remote::screen_stream::Watch;

#[cfg(target_os = "macos")]
use super::audio_mac as os;
#[cfg(windows)]
use super::audio_win as os;
use super::{contains, Rect};

/// 부팅 때 한 번 — 이 기기 화면을 보는 쪽이 소리도 받게 한다.
pub(crate) fn install() {
    kasa_remote::screen_stream::audio::install(Arc::new(os::Source));
}

/// 이 앱이 떠 있는 동안 소리를 끈 기기 — 칸을 넘겼다 돌아와 다시 붙어도 꺼진 채로 둔다.
static MUTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn muted(label: &str) -> bool {
    MUTED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|l| l == label)
}

fn remember(label: &str, on: bool) {
    let mut m = MUTED.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|l| l != label);
    if !on {
        m.push(label.to_string());
    }
}

/// 검증 리그는 받은 소리를 장치로 내되 소리 없음으로 낸다 — 같은 맥 리그 둘이면 보는 쪽이 튼 소리를 보내는 쪽이 다시
/// 떠 되먹임이 돈다(보내는 쪽은 제 소리만 뺀다). 사람 귀에도 두 번 들리지 않는다.
fn silent() -> bool {
    crate::verification_run() && std::env::var_os("KASATERM_TEST_SCREEN_AUDIO_SILENT").is_some()
}

/// 칸 하나의 소리. 받은 덩이를 지터 버퍼에 모으고, 첫 덩이가 오면 OS 출력을 열어 거기서 꺼내 튼다.
pub(crate) struct Player {
    jitter: Arc<Mutex<Jitter>>,
    output: Mutex<Option<os::Output>>,
    enabled: AtomicBool,
    /// 그 기기가 소리를 보낼 수 있다 — 압축 정보의 `audio`. 옛 판이면 알약을 안 그린다.
    remote: AtomicBool,
    opening: AtomicBool,
    /// 출력을 못 열었다 — 다시 켤 때까지 덩이마다 다시 열지 않는다.
    failed: AtomicBool,
    /// 지난번 `stats` 뒤 받은 덩이들의 나이(소리가 난 때 → 받은 때) 합과 수 — 검증 리그가 읽는다.
    ages: Mutex<(f64, u32)>,
}

impl Player {
    pub(crate) fn set_remote(&self, can: bool) {
        self.remote.store(can, Ordering::Relaxed);
    }

    /// 받은 덩이 하나(연결 스레드). 출력 열기는 장치를 기다릴 수 있어 따로 스레드에서 한다.
    pub(crate) fn push(self: &Arc<Self>, chunk: Chunk, t_recv: f64, offset: f64) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let age = t_recv + offset - chunk.t_cap;
        {
            let mut a = self.ages.lock().unwrap_or_else(|e| e.into_inner());
            a.0 += age;
            a.1 += 1;
        }
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).push(chunk, age);
        let idle = self.output.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        if !idle || self.failed.load(Ordering::Relaxed) || self.opening.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        let spawned = std::thread::Builder::new().name("screen-audio-open".into()).spawn(move || {
            let opened = os::Output::start(me.jitter.clone(), silent());
            let mut slot = me.output.lock().unwrap_or_else(|e| e.into_inner());
            match opened {
                Ok(out) if me.enabled.load(Ordering::Relaxed) => *slot = Some(out),
                Ok(_) => {}
                Err(e) => {
                    eprintln!("[screen-audio] 소리 출력을 못 연다: {e}");
                    me.failed.store(true, Ordering::Relaxed);
                }
            }
            drop(slot);
            me.opening.store(false, Ordering::Release);
        });
        if spawned.is_err() {
            self.opening.store(false, Ordering::Release);
        }
    }

    fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if on {
            self.failed.store(false, Ordering::Relaxed);
            return;
        }
        drop(self.output.lock().unwrap_or_else(|e| e.into_inner()).take());
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).reset();
    }
}

/// 화면 보기의 소리 — `ScreenView.audio`. 끄면 이쪽 틀기를 멈추고 그 기기에도 그만 보내라고 알린다.
pub(crate) struct ViewAudio {
    label: String,
    pub on: bool,
    player: Arc<Player>,
    /// 「소리」 알약을 그린 자리 — 칠할 때 채운다.
    pub pill: Option<Rect>,
}

impl ViewAudio {
    pub(crate) fn new(label: &str) -> Self {
        let on = !muted(label);
        let player = Arc::new(Player {
            jitter: Arc::new(Mutex::new(Jitter::default())),
            output: Mutex::new(None),
            enabled: AtomicBool::new(on),
            remote: AtomicBool::new(false),
            opening: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            ages: Mutex::new((0.0, 0)),
        });
        Self { label: label.to_string(), on, player, pill: None }
    }

    /// 연결 사건을 받는 쪽이 쥐는 손잡이.
    pub(crate) fn player(&self) -> Arc<Player> {
        self.player.clone()
    }

    /// 알약을 그릴지 — 그 기기가 소리를 보낼 수 있는 판일 때만.
    pub(crate) fn shown(&self) -> bool {
        self.player.remote.load(Ordering::Relaxed)
    }

    pub(crate) fn text(&self) -> &'static str {
        if self.on {
            "소리"
        } else {
            "소리 끔"
        }
    }

    pub(crate) fn on_pill(&self, p: (f64, f64)) -> bool {
        self.pill.is_some_and(|r| contains(r, p))
    }

    pub(crate) fn set(&mut self, on: bool, watch: Option<&Watch>) {
        self.on = on;
        remember(&self.label, on);
        self.player.set_enabled(on);
        if let Some(w) = watch {
            w.set_audio(on);
        }
    }

    /// 검증 리그 로그 — 받은 덩이·버린 덩이·모아 둔 양·받은 소리 크기(지난번 뒤 가장 큰 진폭).
    pub(crate) fn stats(&self) -> String {
        let (s, buffered, peak) = {
            let mut j = self.player.jitter.lock().unwrap_or_else(|e| e.into_inner());
            (j.stats, j.buffered_ms(), j.take_peak())
        };
        let out = if self.player.output.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
            "open"
        } else if self.player.failed.load(Ordering::Relaxed) {
            "failed"
        } else {
            "none"
        };
        let db = if peak == 0 { -99.0 } else { 20.0 * (f64::from(peak) / 32768.0).log10() };
        let (sum, n) = std::mem::take(&mut *self.player.ages.lock().unwrap_or_else(|e| e.into_inner()));
        let age = if n == 0 { 0.0 } else { sum / f64::from(n) * 1000.0 };
        format!(
            "on={} remote={} out={out} recv={} late={} lost={} trimmed={} underruns={} age={age:.1}ms buf={buffered:.0}ms peak={db:.0}dBFS",
            self.on,
            self.shown(),
            s.received,
            s.late,
            s.lost,
            s.trimmed,
            s.underruns
        )
    }
}

impl Drop for ViewAudio {
    fn drop(&mut self) {
        self.player.set_enabled(false);
    }
}

// ---------------------------------------------------------------- 형식 바꾸기

/// 선형 보간으로 표본율을 바꾼다. 남의 화면 소리를 따라 듣는 용도라 이것으로 넉넉하다 — 둘이 같은 율(48kHz)이면 한 장
/// 늦을 뿐 그대로 지난다.
#[derive(Clone, Debug)]
pub(super) struct Linear {
    /// 출력 한 장마다 입력에서 나아가는 장 수.
    step: f64,
    /// 다음 출력 장의 자리(`prev` 에서 센 입력 장 수, 0–1).
    t: f64,
    prev: [f32; 2],
    cur: [f32; 2],
    primed: bool,
}

impl Linear {
    pub(super) fn new(from: u32, to: u32) -> Self {
        Self { step: f64::from(from.max(1)) / f64::from(to.max(1)), t: 0.0, prev: [0.0; 2], cur: [0.0; 2], primed: false }
    }

    fn at(&self) -> [f32; 2] {
        let t = self.t as f32;
        [self.prev[0] + (self.cur[0] - self.prev[0]) * t, self.prev[1] + (self.cur[1] - self.prev[1]) * t]
    }

    /// 입력 한 장을 넣고 그 사이에 떨어지는 출력 장들을 붙인다 — 뜨기(장치 율 → 48kHz).
    pub(super) fn push(&mut self, frame: [f32; 2], out: &mut Vec<[f32; 2]>) {
        if !self.primed {
            self.cur = frame;
            self.primed = true;
            return;
        }
        self.prev = self.cur;
        self.cur = frame;
        while self.t < 1.0 {
            out.push(self.at());
            self.t += self.step;
        }
        self.t -= 1.0;
    }

    /// 출력 `n` 장을 붙인다. 입력은 모자랄 때마다 `next` 로 당긴다 — 틀기(48kHz → 장치 율).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(super) fn pull(&mut self, n: usize, mut next: impl FnMut() -> [f32; 2], out: &mut Vec<[f32; 2]>) {
        if !self.primed {
            self.prev = next();
            self.cur = next();
            self.primed = true;
        }
        for _ in 0..n {
            while self.t >= 1.0 {
                self.prev = self.cur;
                self.cur = next();
                self.t -= 1.0;
            }
            out.push(self.at());
            self.t += self.step;
        }
    }
}

fn quantize(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// 뜬 장들(왼쪽·오른쪽 f32, 장치 율)을 화면 스트림 소리 형식(48kHz 스테레오 s16)으로.
pub(super) struct ToWire {
    lin: Linear,
    frames: Vec<[f32; 2]>,
    pcm: Vec<i16>,
}

impl ToWire {
    pub(super) fn new(rate: u32) -> Self {
        Self { lin: Linear::new(rate, RATE), frames: Vec::new(), pcm: Vec::new() }
    }

    pub(super) fn convert(&mut self, input: &[[f32; 2]]) -> &[i16] {
        self.frames.clear();
        for f in input {
            self.lin.push(*f, &mut self.frames);
        }
        self.pcm.clear();
        self.pcm.extend(self.frames.iter().flat_map(|[l, r]| [quantize(*l), quantize(*r)]));
        &self.pcm
    }
}

/// 지터 버퍼의 48kHz 스테레오를 장치 율로 — 5ms 씩 꺼내 표본율을 맞춘다(Windows 틀기. macOS 는 AudioUnit 이 바꾼다).
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) struct FromWire {
    lin: Linear,
    block: Vec<i16>,
    at: usize,
    frames: Vec<[f32; 2]>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl FromWire {
    pub(super) fn new(rate: u32) -> Self {
        Self { lin: Linear::new(RATE, rate), block: Vec::new(), at: 0, frames: Vec::new() }
    }

    pub(super) fn fill(&mut self, n: usize, jitter: &Mutex<Jitter>) -> &[[f32; 2]] {
        let Self { lin, block, at, frames } = self;
        frames.clear();
        lin.pull(
            n,
            || {
                if *at >= block.len() {
                    block.resize(CHUNK_FRAMES * CHANNELS, 0);
                    jitter.lock().unwrap_or_else(|e| e.into_inner()).pull(block);
                    *at = 0;
                }
                let f = [f32::from(block[*at]) / 32768.0, f32::from(block[*at + 1]) / 32768.0];
                *at += CHANNELS;
                f
            },
            frames,
        );
        frames
    }
}

/// 장치 섞기 형식의 한 표본.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sample {
    F32,
    I16,
    I32,
}

/// 장치 섞기 형식(Windows `GetMixFormat`).
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(super) struct Mix {
    pub rate: u32,
    pub channels: usize,
    pub sample: Sample,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Mix {
    fn sample_bytes(&self) -> usize {
        if self.sample == Sample::I16 {
            2
        } else {
            4
        }
    }

    pub(super) fn frame_bytes(&self) -> usize {
        self.sample_bytes() * self.channels
    }

    fn read(&self, b: &[u8]) -> f32 {
        match self.sample {
            Sample::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Sample::I16 => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
            Sample::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
        }
    }

    fn write(&self, x: f32, b: &mut [u8]) {
        match self.sample {
            Sample::F32 => b[..4].copy_from_slice(&x.clamp(-1.0, 1.0).to_le_bytes()),
            Sample::I16 => b[..2].copy_from_slice(&quantize(x).to_le_bytes()),
            Sample::I32 => b[..4].copy_from_slice(&((f64::from(x.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32).to_le_bytes()),
        }
    }

    /// 장치 장들 → 왼쪽·오른쪽. 한 채널이면 둘에 같이, 셋 이상이면 앞 둘(섞기 형식의 앞 둘은 앞 왼쪽·오른쪽이다).
    pub(super) fn read_stereo(&self, bytes: &[u8], out: &mut Vec<[f32; 2]>) {
        out.clear();
        let s = self.sample_bytes();
        for f in bytes.chunks_exact(self.frame_bytes()) {
            let l = self.read(&f[..s]);
            let r = if self.channels > 1 { self.read(&f[s..2 * s]) } else { l };
            out.push([l, r]);
        }
    }

    /// 왼쪽·오른쪽 → 장치 장들. 한 채널이면 둘의 가운데, 셋째 채널부터는 소리 없음.
    pub(super) fn write_stereo(&self, frames: &[[f32; 2]], out: &mut [u8]) {
        out.fill(0);
        let s = self.sample_bytes();
        for (f, [l, r]) in out.chunks_exact_mut(self.frame_bytes()).zip(frames) {
            if self.channels == 1 {
                self.write((l + r) / 2.0, &mut f[..s]);
            } else {
                self.write(*l, &mut f[..s]);
                self.write(*r, &mut f[s..2 * s]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<[f32; 2]> {
        (0..n).map(|i| [i as f32 / n as f32, -(i as f32) / n as f32]).collect()
    }

    #[test]
    fn the_same_rate_passes_through_one_frame_late() {
        let mut w = ToWire::new(RATE);
        let pcm = w.convert(&ramp(480)).to_vec();
        assert_eq!(pcm.len(), 479 * CHANNELS);
        assert_eq!(pcm[2], quantize(1.0 / 480.0));
        assert_eq!(pcm[3], quantize(-1.0 / 480.0));
    }

    #[test]
    fn a_44k1_device_comes_out_at_48k_with_the_same_length_and_shape() {
        let mut w = ToWire::new(44_100);
        let mut total = 0;
        let mut last = Vec::new();
        for _ in 0..100 {
            let pcm = w.convert(&vec![[0.5, -0.25]; 441]);
            total += pcm.len() / CHANNELS;
            last = pcm.to_vec();
        }
        assert!((total as i64 - 48_000).abs() <= 2, "1초는 1초로: {total}");
        assert!(last.chunks_exact(2).all(|f| f[0] == quantize(0.5) && f[1] == quantize(-0.25)));
    }

    #[test]
    fn pulling_for_a_44k1_device_takes_48k_frames_at_the_right_pace() {
        let mut lin = Linear::new(RATE, 44_100);
        let mut taken = 0usize;
        let mut out = Vec::new();
        lin.pull(44_100, || {
            taken += 1;
            [taken as f32, 0.0]
        }, &mut out);
        assert_eq!(out.len(), 44_100);
        assert!((taken as i64 - 48_000).abs() <= 2, "장치 1초에 48k 장을 당긴다: {taken}");
        assert!(out.windows(2).all(|w| w[1][0] > w[0][0]), "차례를 지킨다");
    }

    #[test]
    fn from_wire_drains_the_jitter_buffer_in_5ms_blocks() {
        let jitter = Mutex::new(Jitter::default());
        for seq in 0..20 {
            jitter.lock().unwrap().push(Chunk { seq, t_cap: 0.0, pcm: vec![16384; CHUNK_FRAMES * CHANNELS] }, 0.0);
        }
        let mut from = FromWire::new(RATE);
        let frames = from.fill(480, &jitter).to_vec();
        assert_eq!(frames.len(), 480);
        assert!(frames[2..].iter().all(|f| (f[0] - 0.5).abs() < 1e-6 && (f[1] - 0.5).abs() < 1e-6));
        assert!((jitter.lock().unwrap().buffered_ms() - (100.0 - 15.0)).abs() < 0.1, "5ms 씩 꺼낸다");
    }

    #[test]
    fn device_formats_map_to_stereo_and_back() {
        let mix = Mix { rate: 48_000, channels: 6, sample: Sample::F32 };
        let mut bytes = Vec::new();
        for i in 0..4 {
            for ch in 0..6 {
                bytes.extend_from_slice(&(i as f32 * 0.1 + ch as f32 * 0.01).to_le_bytes());
            }
        }
        let mut st = Vec::new();
        mix.read_stereo(&bytes, &mut st);
        assert_eq!(st.len(), 4);
        assert!((st[2][0] - 0.2).abs() < 1e-6 && (st[2][1] - 0.21).abs() < 1e-6, "앞 왼쪽·오른쪽");
        let mut out = vec![0xAA; 4 * mix.frame_bytes()];
        mix.write_stereo(&st, &mut out);
        let back = |frame: usize, ch: usize| f32::from_le_bytes(out[frame * 24 + ch * 4..][..4].try_into().unwrap());
        assert!((back(2, 1) - 0.21).abs() < 1e-6 && back(2, 2) == 0.0 && back(3, 5) == 0.0, "나머지 채널은 소리 없음");

        let mono = Mix { rate: 44_100, channels: 1, sample: Sample::I16 };
        let mut st = Vec::new();
        mono.read_stereo(&16384i16.to_le_bytes(), &mut st);
        assert_eq!(st, vec![[0.5, 0.5]]);
        let mut out = vec![0; 2];
        mono.write_stereo(&[[1.0, 0.0]], &mut out);
        assert_eq!(i16::from_le_bytes([out[0], out[1]]), quantize(0.5));

        let wide = Mix { rate: 48_000, channels: 2, sample: Sample::I32 };
        let mut st = Vec::new();
        wide.read_stereo(&[(i32::MIN / 2).to_le_bytes(), (i32::MAX / 4).to_le_bytes()].concat(), &mut st);
        assert!((st[0][0] + 0.5).abs() < 1e-6 && (st[0][1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn muting_a_machine_is_remembered_until_it_is_turned_back_on() {
        let mut a = ViewAudio::new("rig-audio-test");
        assert!(a.on && a.text() == "소리");
        a.set(false, None);
        assert_eq!(a.text(), "소리 끔");
        drop(a);
        let mut b = ViewAudio::new("rig-audio-test");
        assert!(!b.on, "다시 붙어도 꺼진 채로");
        b.player().push(Chunk { seq: 0, t_cap: 0.0, pcm: vec![1; CHUNK_FRAMES * CHANNELS] }, 0.0, 0.0);
        assert_eq!(b.player.jitter.lock().unwrap().stats.received, 0, "끈 동안 받은 덩이는 모으지 않는다");
        b.set(true, None);
        assert!(ViewAudio::new("rig-audio-test").on);
    }
}
