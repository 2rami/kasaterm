//! 인라인 오디오·비디오 카드의 네이티브 재생기와 파일 검사.
//!
//! macOS 는 AVFoundation(AVPlayer + AVPlayerItemVideoOutput)에 맡긴다 — 진행은 오디오 장치
//! 시계를 따르고, 프레임은 그 시계의 지금 시각에 맞는 것만 내준다. 우리는 프레임을 RGBA 로
//! 옮기고 트랙의 표시 회전만 적용한다. GPU 갱신·카드 UI 는 부르는 쪽이 맡는다.
//!
//! `Player` 는 메인 스레드에서만 만들고 쓴다(AVPlayer 가 메인 스레드 전용이라 타입이 `!Send`).
//! `probe` 는 파일을 읽느라 막힐 수 있으니 GUI 스레드가 아닌 곳에서 부른다.

use std::path::Path;

#[cfg(target_os = "macos")]
#[path = "media_player/macos.rs"]
mod imp;

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Frame, Playback, Probe};
    use std::path::Path;

    const UNSUPPORTED: &str = "native media playback is only available on macOS";

    pub struct Player;

    impl Player {
        pub fn open(_path: &Path) -> Result<Self, String> {
            Err(UNSUPPORTED.to_string())
        }
        pub fn play(&mut self) {}
        pub fn pause(&mut self) {}
        pub fn seek(&mut self, _seconds: f64) {}
        pub fn set_volume(&mut self, _volume: f32) {}
        pub fn state(&self) -> Playback {
            Playback {
                error: Some(UNSUPPORTED.to_string()),
                ..Playback::default()
            }
        }
        pub fn take_frame(&mut self) -> Option<Frame> {
            None
        }
    }

    pub fn probe(_path: &Path) -> Result<Probe, String> {
        Err(UNSUPPORTED.to_string())
    }
}

/// `probe` 가 만드는 포스터의 긴 변 상한. 레지스트리 계약(1600px)과 같다.
pub const POSTER_MAX_EDGE: u32 = 1600;
/// 레지스트리 계약의 포스터 PNG 크기 상한.
pub const POSTER_MAX_BYTES: usize = 12 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playback {
    pub position: f64,
    pub duration: Option<f64>,
    pub playing: bool,
    pub buffering: bool,
    pub ended: bool,
    /// 위치 찾기(열기 직후 첫 프레임 포함)가 끝나 새 위치의 프레임이 나올 때까지 true.
    /// 멈춘 상태에서도 이동안은 렌더 루프를 돌려 `take_frame` 을 계속 불러야 그림이 따라온다.
    /// 소리만 있는 파일은 위치 찾기 완료까지, 새 프레임이 안 나오면 5초 뒤 false 로 내려온다.
    pub seeking: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// 위에서 아래로, 줄 사이 여백 없이 `width * 4` 바이트씩. 표시 회전이 이미 적용돼 있다.
    pub rgba: Vec<u8>,
    /// 새 프레임마다 1씩 는다. 같은 값이면 텍스처를 다시 올릴 필요가 없다.
    pub serial: u64,
}

/// 레지스트리 `Probe` 와 같은 필드에 트랙 유무를 더했다. 크기는 표시 회전을 적용한 값이다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// PNG. 긴 변 `POSTER_MAX_EDGE` 이하.
    pub poster: Option<Vec<u8>>,
    pub has_audio: bool,
    pub has_video: bool,
}

/// 파일 하나를 재생한다. 열린 직후에는 멈춰 있고 `play` 를 불러야 소리가 난다.
/// 떨어뜨리면 재생을 멈추고 AVFoundation 객체를 놓는다.
pub struct Player {
    inner: imp::Player,
}

impl Player {
    /// 열기는 막히지 않는다. 읽기 실패·지원하지 않는 형식은 `state().error` 로 나중에 드러난다.
    pub fn open(path: &Path) -> Result<Self, String> {
        imp::Player::open(path).map(|inner| Self { inner })
    }

    /// 끝까지 재생한 뒤 부르면 처음부터 다시 튼다.
    pub fn play(&mut self) {
        self.inner.play();
    }

    pub fn pause(&mut self) {
        self.inner.pause();
    }

    /// 초 단위. 길이 밖은 잘라 맞춘다. 멈춘 상태면 그 자리의 프레임이 `take_frame` 으로 온다.
    pub fn seek(&mut self, seconds: f64) {
        self.inner.seek(seconds);
    }

    /// 0.0..=1.0
    pub fn set_volume(&mut self, volume: f32) {
        self.inner.set_volume(volume);
    }

    pub fn state(&self) -> Playback {
        self.inner.state()
    }

    /// 지난 호출 뒤 표시할 새 프레임이 있을 때만 돌려준다. 렌더 루프마다 불러도 된다.
    pub fn take_frame(&mut self) -> Option<Frame> {
        self.inner.take_frame()
    }

    /// 떨어뜨린 뒤 AVPlayer 가 실제로 해제됐는지 확인하는 검증용 손잡이.
    #[cfg(target_os = "macos")]
    #[doc(hidden)]
    pub fn drop_watch(&self) -> imp::DropWatch {
        self.inner.drop_watch()
    }
}

/// 길이·표시 크기·포스터를 읽는다. 파일을 읽으며 막히므로 GUI 스레드에서 부르지 않는다.
pub fn probe(path: &Path) -> Result<Probe, String> {
    imp::probe(path)
}

/// 멈췄고 끝에 닿았으면 끝난 것이다. AVPlayer 는 끝에서 스스로 멈추고(`Pause`) 위치를 길이에 둔다.
fn playback_ended(position: f64, duration: Option<f64>, playing: bool) -> bool {
    const END_SLACK: f64 = 0.05;
    match duration {
        Some(d) if d > 0.0 => !playing && position >= d - END_SLACK,
        _ => false,
    }
}

/// 트랙의 `preferredTransform` 중 회전·뒤집기만 남긴 것. 위가 y=0 인 픽셀 좌표에서
/// 출력 = `[a c; b d] · 입력` 이고, 평행이동은 결과가 0 에서 시작하도록 다시 맞춘다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Orientation {
    a: i8,
    b: i8,
    c: i8,
    d: i8,
}

impl Orientation {
    const IDENTITY: Self = Self {
        a: 1,
        b: 0,
        c: 0,
        d: 1,
    };

    /// 90° 단위 회전·뒤집기가 아니면(기울임·임의 각도) 그대로 둔다 — 틀린 방향보다 낫다.
    fn from_affine(a: f64, b: f64, c: f64, d: f64) -> Self {
        fn unit(v: f64) -> Option<i8> {
            if (v - 1.0).abs() < 1e-3 {
                Some(1)
            } else if (v + 1.0).abs() < 1e-3 {
                Some(-1)
            } else if v.abs() < 1e-3 {
                Some(0)
            } else {
                None
            }
        }
        let (Some(a), Some(b), Some(c), Some(d)) = (unit(a), unit(b), unit(c), unit(d)) else {
            return Self::IDENTITY;
        };
        let straight = a != 0 && d != 0 && b == 0 && c == 0;
        let swapped = b != 0 && c != 0 && a == 0 && d == 0;
        if straight || swapped {
            Self { a, b, c, d }
        } else {
            Self::IDENTITY
        }
    }

    fn swaps_axes(self) -> bool {
        self.a == 0
    }

    fn output_size(self, width: u32, height: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (height, width)
        } else {
            (width, height)
        }
    }

    /// BGRA 원본(줄마다 `stride` 바이트)을 회전한 빽빽한 RGBA 로 옮긴다. 알파는 255 로 채운다
    /// — 비디오 출력의 알파는 의미가 없고 0 으로 오는 디코더도 있다.
    fn bgra_to_rgba(self, src: &[u8], stride: usize, width: u32, height: u32) -> Option<Vec<u8>> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || stride < w * 4 || src.len() < stride * (h - 1) + w * 4 {
            return None;
        }
        let (ow, oh) = self.output_size(width, height);
        let ow = ow as usize;
        let mut out = vec![0u8; ow * oh as usize * 4];
        if self == Self::IDENTITY {
            for (y, dst_row) in out.chunks_exact_mut(w * 4).enumerate() {
                let src_row = &src[y * stride..y * stride + w * 4];
                for (d, s) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                    d.copy_from_slice(&[s[2], s[1], s[0], 255]);
                }
            }
            return Some(out);
        }
        fn term(k: i8, v: usize, n: usize) -> usize {
            match k {
                1 => v,
                -1 => n - 1 - v,
                _ => 0,
            }
        }
        for y in 0..h {
            let src_row = &src[y * stride..y * stride + w * 4];
            for x in 0..w {
                let dx = term(self.a, x, w) + term(self.c, y, h);
                let dy = term(self.b, x, w) + term(self.d, y, h);
                let s = &src_row[x * 4..x * 4 + 4];
                let o = (dy * ow + dx) * 4;
                out[o..o + 4].copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x1 BGRA: 왼쪽 빨강, 오른쪽 파랑. 줄 끝에 여백 4바이트.
    fn red_blue() -> (Vec<u8>, usize) {
        (vec![0, 0, 255, 0, 255, 0, 0, 0, 9, 9, 9, 9], 12)
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    #[test]
    fn identity_swizzles_and_drops_row_padding() {
        let (src, stride) = red_blue();
        let out = Orientation::IDENTITY
            .bgra_to_rgba(&src, stride, 2, 1)
            .unwrap();
        assert_eq!(out, [RED, BLUE].concat());
    }

    #[test]
    fn quarter_turn_clockwise_puts_left_on_top() {
        // 아이폰 세로 영상의 transform(0, 1, -1, 0, h, 0): 왼쪽 → 위.
        let o = Orientation::from_affine(0.0, 1.0, -1.0, 0.0);
        let (src, stride) = red_blue();
        assert_eq!(o.output_size(2, 1), (1, 2));
        assert_eq!(
            o.bgra_to_rgba(&src, stride, 2, 1).unwrap(),
            [RED, BLUE].concat()
        );
    }

    #[test]
    fn quarter_turn_counter_clockwise_puts_left_on_bottom() {
        let o = Orientation::from_affine(0.0, -1.0, 1.0, 0.0);
        let (src, stride) = red_blue();
        assert_eq!(
            o.bgra_to_rgba(&src, stride, 2, 1).unwrap(),
            [BLUE, RED].concat()
        );
    }

    #[test]
    fn half_turn_reverses() {
        let o = Orientation::from_affine(-1.0, 0.0, 0.0, -1.0);
        let (src, stride) = red_blue();
        assert_eq!(o.output_size(2, 1), (2, 1));
        assert_eq!(
            o.bgra_to_rgba(&src, stride, 2, 1).unwrap(),
            [BLUE, RED].concat()
        );
    }

    #[test]
    fn rotation_maps_every_corner_of_a_non_square_frame() {
        // 3x2, 픽셀마다 B 채널에 번호. 시계 방향 90° 뒤 2x3 이 되고 왼쪽 아래가 왼쪽 위로 온다.
        let src: Vec<u8> = (0..6u8).flat_map(|i| [i, 0, 0, 0]).collect();
        let o = Orientation::from_affine(0.0, 1.0, -1.0, 0.0);
        let out = o.bgra_to_rgba(&src, 12, 3, 2).unwrap();
        let ids: Vec<u8> = out.chunks_exact(4).map(|p| p[2]).collect();
        assert_eq!(ids, [3, 0, 4, 1, 5, 2]);
    }

    #[test]
    fn skew_or_odd_angles_fall_back_to_identity() {
        assert_eq!(
            Orientation::from_affine(0.7, 0.7, -0.7, 0.7),
            Orientation::IDENTITY
        );
        assert_eq!(
            Orientation::from_affine(1.0, 1.0, 0.0, 1.0),
            Orientation::IDENTITY
        );
    }

    #[test]
    fn short_buffers_are_rejected() {
        assert!(Orientation::IDENTITY
            .bgra_to_rgba(&[0; 7], 8, 2, 1)
            .is_none());
        assert!(Orientation::IDENTITY
            .bgra_to_rgba(&[0; 8], 4, 2, 1)
            .is_none());
        assert!(Orientation::IDENTITY.bgra_to_rgba(&[], 0, 0, 0).is_none());
    }

    #[test]
    fn ended_needs_pause_at_the_end() {
        assert!(playback_ended(3.0, Some(3.0), false));
        assert!(playback_ended(2.97, Some(3.0), false));
        assert!(!playback_ended(3.0, Some(3.0), true));
        assert!(!playback_ended(1.0, Some(3.0), false));
        assert!(!playback_ended(0.0, None, false));
    }
}
