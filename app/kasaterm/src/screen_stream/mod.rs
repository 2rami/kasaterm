//! 화면 스트림 — 다른 기기 화면을 하드웨어 영상 압축으로 카사텀 칸 안에 본다. 계약·실측은 `docs/screen-stream.md`.
//! 틀과 흐름은 `kasa_remote::screen_stream`, 여기는 macOS 의 화면 뜨기(SCK)·압축·풀기(VT)다. 다른 OS 는 아직
//! 보내지도 보지도 않는다 — 화면 거울이 JPEG 웹 칸으로 연다.

#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
mod codec;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod scroll_probe;
#[cfg(target_os = "macos")]
mod view;
mod input;
mod pane;

pub(crate) use pane::paint;

#[cfg(target_os = "macos")]
pub(crate) use view::ScreenView;

/// 호스트 시계(초). SCK 장의 시각과 같은 시계다.
#[cfg(target_os = "macos")]
pub(crate) fn host_now() -> f64 {
    unsafe { objc2_core_media::CMClock::host_time_clock().time().seconds() }
}

/// 부팅 때 한 번 — 카사넷 화면 입구를 연다. 화면 기록 권한은 붙는 때 따진다.
#[cfg(target_os = "macos")]
pub(crate) fn install() {
    kasa_remote::screen_stream::install(std::sync::Arc::new(host::Host));
}

/// 그 기기를 네이티브 칸으로 볼 수 있나 — 이 판이 보고, 그 기기가 보내는 판이고, 카사넷 직통·믿는 중계일 때.
pub(crate) fn can_view(base: &str) -> bool {
    cfg!(target_os = "macos")
        && kasa_device::kasanet::screen_capable(base)
        && kasa_device::kasanet::path(base).is_some_and(|(p, _)| p != "ssh")
}

/// 칸 위에 그린 자리(LOGICAL px) — 누름이 어디에 떨어졌는지 가른다.
#[derive(Clone, Copy, Default)]
pub(crate) struct Layout {
    /// 장이 그려진 사각형(비율 맞춤 뒤).
    pub image: (f32, f32, f32, f32),
    /// 「조작」 단추.
    pub toggle: (f32, f32, f32, f32),
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn contains((x, y, w, h): (f32, f32, f32, f32), (px, py): (f64, f64)) -> bool {
    let (px, py) = (px as f32, py as f32);
    px >= x && px < x + w && py >= y && py < y + h
}

#[cfg(not(target_os = "macos"))]
pub(crate) struct ScreenView {
    pub label: String,
    pub upload_ms: f64,
    pub uploads: u64,
    pub control: bool,
    pub layout: Option<Layout>,
    pub buttons: u8,
    pub preedit: String,
    pub zero_copy: bool,
    pub hidden_since: Option<std::time::Instant>,
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
impl ScreenView {
    pub(crate) fn open(_label: &str, _base: &str, _wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        None
    }
    pub(crate) fn new_surface(&mut self) -> Option<(*mut std::ffi::c_void, u32, u32)> {
        None
    }
    pub(crate) fn copy_latest(&self, _f: impl FnOnce(&[u8], u32, u32, u32)) {}
    pub(crate) fn note_upload(&mut self, _took: std::time::Duration, _zero_copy: bool) {}
    pub(crate) fn has_new_frame(&self) -> bool {
        false
    }
    pub(crate) fn has_frame(&self) -> bool {
        false
    }
    pub(crate) fn frame_size(&self) -> (u32, u32) {
        (0, 0)
    }
    pub(crate) fn status(&self) -> String {
        String::new()
    }
    pub(crate) fn hud(&self) -> String {
        String::new()
    }
    pub(crate) fn input_note(&self) -> Option<String> {
        None
    }
    pub(crate) fn on_toggle(&self, _p: (f64, f64)) -> bool {
        false
    }
    pub(crate) fn on_image(&self, _p: (f64, f64)) -> bool {
        false
    }
    pub(crate) fn pointer(&mut self, _p: (f64, f64), _buttons: u8, _mods: u8) {}
    pub(crate) fn scroll(&self, _p: (f64, f64), _dx: f64, _dy: f64) {}
    pub(crate) fn key(&self, _code: &str, _mods: u8) {}
    pub(crate) fn text(&self, _s: &str) {}
}
