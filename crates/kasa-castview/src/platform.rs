//! 풀기·소리 틀기의 OS 부분. 맥·Windows 는 `kasa_cast_host` 것이고, 그 밖은 비어 있다 — 보기가 열리지 않으니 불리지 않지만
//! 보기 낱말(`ScreenView`)은 어디서나 같은 꼴로 컴파일되게.

#[cfg(any(target_os = "macos", windows))]
pub use kasa_cast_host::audio::Output;
#[cfg(any(target_os = "macos", windows))]
pub use kasa_cast_host::{zero_copy_result, Picture, VideoDecoder};

#[cfg(not(any(target_os = "macos", windows)))]
mod none {
    use std::sync::{Arc, Mutex};

    use kasa_cast::audio::Jitter;

    #[derive(Clone)]
    pub enum Picture {}

    impl Picture {
        pub fn size(&self) -> (u32, u32) {
            match *self {}
        }
        pub fn surface(&self) -> (*mut std::ffi::c_void, u64) {
            match *self {}
        }
        pub fn with_bgra<R>(&self, _f: impl FnOnce(&[u8], u32, u32, u32) -> R) -> Option<R> {
            match *self {}
        }
    }

    pub struct VideoDecoder;

    impl VideoDecoder {
        pub const CODEC: &'static str = "h264";
        pub fn new() -> Self {
            Self
        }
        pub fn decode(&mut self, _frame: &mut kasa_cast::Frame) -> Result<Option<Picture>, String> {
            Err("이 OS 에는 풀기가 없다".into())
        }
    }

    pub fn zero_copy_result(_ok: bool) {}

    pub struct Output;

    impl Output {
        pub fn start(_jitter: Arc<Mutex<Jitter>>, _silent: bool) -> Result<Self, String> {
            Err("이 OS 에는 소리 출력이 없다".into())
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub use none::{zero_copy_result, Output, Picture, VideoDecoder};
