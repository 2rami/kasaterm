//! 시스템 소리의 틀 바꾸기와 이 기기 소리 허브 꽂기 — 뜨기·틀기는 `audio_mac.rs`·`audio_win.rs`, 덩이·허브·지터 버퍼는
//! `kasa_cast::audio`(`docs/screen-stream.md` 「S4 소리」).

use std::sync::{Arc, Mutex};

use kasa_cast::audio::{Jitter, CHANNELS, CHUNK_FRAMES, RATE};

#[cfg(target_os = "macos")]
use crate::audio_mac as os;
#[cfg(windows)]
use crate::audio_win as os;

/// 부팅 때 한 번 — 이 기기 화면을 보는 쪽이 소리도 받게 한다. 지난 실행이 끈 채 죽었으면 스피커를 먼저 되켠다.
#[cfg(any(target_os = "macos", windows))]
pub fn install() {
    crate::source_mute::restore_after_crash();
    kasa_cast::audio::install(Arc::new(Speakers));
}

/// 소리를 뜨는 동안 이 기기 스피커를 끈다 — 보는 쪽에서만 난다(`source_mute.rs`). 마지막 구독이 떠나 뜨기를 멈추면 되켠다.
#[cfg(any(target_os = "macos", windows))]
struct Speakers;

#[cfg(any(target_os = "macos", windows))]
impl kasa_cast::audio::Source for Speakers {
    fn start(&self, out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String> {
        let running = os::Source.start(out)?;
        Ok(Box::new((running, crate::source_mute::mute())))
    }
}

/// 보는 쪽 출력 — 지터 버퍼에서 꺼내 기본 출력 장치로 튼다. `silent` 면 소리 없이 낸다(같은 기기 리그의 되먹임 막기).
#[cfg(any(target_os = "macos", windows))]
pub struct Output(#[allow(dead_code)] os::Output);

#[cfg(any(target_os = "macos", windows))]
impl Output {
    pub fn start(jitter: Arc<Mutex<Jitter>>, silent: bool) -> Result<Self, String> {
        os::Output::start(jitter, silent).map(Self)
    }
}

/// 선형 보간으로 표본율을 바꾼다. 남의 화면 소리를 따라 듣는 용도라 이것으로 넉넉하다 — 둘이 같은 율(48kHz)이면 한 장
/// 늦을 뿐 그대로 지난다.
#[derive(Clone, Debug)]
pub struct Linear {
    /// 출력 한 장마다 입력에서 나아가는 장 수.
    step: f64,
    /// 다음 출력 장의 자리(`prev` 에서 센 입력 장 수, 0–1).
    t: f64,
    prev: [f32; 2],
    cur: [f32; 2],
    primed: bool,
}

impl Linear {
    pub fn new(from: u32, to: u32) -> Self {
        Self { step: f64::from(from.max(1)) / f64::from(to.max(1)), t: 0.0, prev: [0.0; 2], cur: [0.0; 2], primed: false }
    }

    fn at(&self) -> [f32; 2] {
        let t = self.t as f32;
        [self.prev[0] + (self.cur[0] - self.prev[0]) * t, self.prev[1] + (self.cur[1] - self.prev[1]) * t]
    }

    /// 입력 한 장을 넣고 그 사이에 떨어지는 출력 장들을 붙인다 — 뜨기(장치 율 → 48kHz).
    pub fn push(&mut self, frame: [f32; 2], out: &mut Vec<[f32; 2]>) {
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
    pub fn pull(&mut self, n: usize, mut next: impl FnMut() -> [f32; 2], out: &mut Vec<[f32; 2]>) {
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
pub struct ToWire {
    lin: Linear,
    frames: Vec<[f32; 2]>,
    pcm: Vec<i16>,
}

impl ToWire {
    pub fn new(rate: u32) -> Self {
        Self { lin: Linear::new(rate, RATE), frames: Vec::new(), pcm: Vec::new() }
    }

    pub fn convert(&mut self, input: &[[f32; 2]]) -> &[i16] {
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
pub struct FromWire {
    lin: Linear,
    block: Vec<i16>,
    at: usize,
    frames: Vec<[f32; 2]>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl FromWire {
    pub fn new(rate: u32) -> Self {
        Self { lin: Linear::new(RATE, rate), block: Vec::new(), at: 0, frames: Vec::new() }
    }

    pub fn fill(&mut self, n: usize, jitter: &Mutex<Jitter>) -> &[[f32; 2]] {
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
pub enum Sample {
    F32,
    I16,
    I32,
}

/// 장치 섞기 형식(Windows `GetMixFormat`).
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct Mix {
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

    pub fn frame_bytes(&self) -> usize {
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
    pub fn read_stereo(&self, bytes: &[u8], out: &mut Vec<[f32; 2]>) {
        out.clear();
        let s = self.sample_bytes();
        for f in bytes.chunks_exact(self.frame_bytes()) {
            let l = self.read(&f[..s]);
            let r = if self.channels > 1 { self.read(&f[s..2 * s]) } else { l };
            out.push([l, r]);
        }
    }

    /// 왼쪽·오른쪽 → 장치 장들. 한 채널이면 둘의 가운데, 셋째 채널부터는 소리 없음.
    pub fn write_stereo(&self, frames: &[[f32; 2]], out: &mut [u8]) {
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
    use kasa_cast::audio::Chunk;

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
}
