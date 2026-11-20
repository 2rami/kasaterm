//! 화면 칸 소리 — 그 기기 시스템 소리를 칸에서 튼다(`docs/screen-stream.md` 「S4 소리」). 틀·허브·지터 버퍼는
//! `kasa_cast::audio`, OS 의 뜨기·틀기는 `kasa_cast_host::audio`, 여기는 칸의 「소리」 알약과 출력을 잇는다.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kasa_remote::screen_stream::audio::{Chunk, Jitter};
use kasa_remote::screen_stream::Watch;

use super::{contains, Rect};

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
    output: Mutex<Option<kasa_cast_host::audio::Output>>,
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
            let opened = kasa_cast_host::audio::Output::start(me.jitter.clone(), silent());
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

#[cfg(test)]
mod tests {
    use kasa_remote::screen_stream::audio::{CHANNELS, CHUNK_FRAMES};

    use super::*;

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
