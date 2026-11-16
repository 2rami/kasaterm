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

#[cfg(not(target_os = "macos"))]
pub(crate) struct ScreenView {
    pub label: String,
    pub upload_ms: f64,
    pub uploads: u64,
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
impl ScreenView {
    pub(crate) fn open(_label: &str, _base: &str, _wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        None
    }
    pub(crate) fn take_new(&mut self, _f: impl FnOnce(&[u8], u32, u32, u32)) {}
    pub(crate) fn has_new_frame(&self) -> bool {
        false
    }
    pub(crate) fn has_frame(&self) -> bool {
        false
    }
    pub(crate) fn status(&self) -> String {
        String::new()
    }
    pub(crate) fn hud(&self) -> String {
        String::new()
    }
}
