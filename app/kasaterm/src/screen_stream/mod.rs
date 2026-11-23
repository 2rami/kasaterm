//! 화면 스트림 — 다른 기기 화면을 하드웨어 영상 압축으로 카사텀 칸 안에 본다. 계약·실측은 `docs/screen-stream.md`.
//! 틀과 흐름은 `kasa_cast`, OS 부분(뜨기·압축·풀기·소리·조작 넣기)은 `kasa_cast_host`, 보기 하나(풀기 일꾼·소리·조작 판정·HUD)는
//! 따로 띄우는 보기 앱과 같이 쓰는 `kasa_castview` 다. 여기는 칸 탭·꺼낸 창과 그 사이의 초점이다.

#[cfg(any(target_os = "macos", windows))]
pub(crate) use kasa_cast_host::restore_speakers;
pub(crate) use kasa_castview::{ScreenView, Surface};

mod desk;
mod input;
mod pane;
mod window;

pub(crate) use desk::open_in_desk;

pub(crate) use pane::paint;
pub(crate) use window::ScreenWindow;

/// 부팅 때 한 번 — 기기 화면 거울(JPEG)·카사넷 화면 입구·이 기기 소리 허브를 연다. 화면 기록 권한은 붙는 때 따진다.
/// 맥은 화면 서비스(도우미)가 있으면 그 화면을 받아 중계한다 — 앱은 스트림에 화면 권한이 필요 없다(`docs/screen-service-split.md` P5).
pub(crate) fn install() {
    #[cfg(target_os = "macos")]
    {
        use kasa_cast_host::local::{Duplex, Hybrid, HybridAudio, HybridCapture, HybridSource};
        let hybrid = std::sync::Arc::new(Hybrid::new(service_socket, kasa_cast_host::capture(), kasa_cast_host::jpeg_source()));
        kasa_remote::screen_mirror::install(std::sync::Arc::new(HybridSource(hybrid.clone())));
        kasa_remote::screen_stream::install(std::sync::Arc::new(HybridCapture(hybrid.clone())));
        kasa_cast_host::audio::install_with(std::sync::Arc::new(HybridAudio(hybrid, kasa_cast_host::audio::speakers())));
        fn service_socket() -> Option<Box<dyn Duplex>> {
            let dir = match std::env::var_os("KASATERM_SCREEN_STATE").filter(|d| !d.is_empty()) {
                Some(d) => std::path::PathBuf::from(d),
                None => std::path::PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/kasaterm/screen"),
            };
            let stream = std::os::unix::net::UnixStream::connect(dir.join("local.sock")).ok()?;
            Some(Box::new(stream))
        }
    }
    #[cfg(windows)]
    {
        kasa_remote::screen_mirror::install(kasa_cast_host::jpeg_source());
        if let Some(capture) = kasa_cast_host::capture() {
            kasa_remote::screen_stream::install(capture);
        }
        kasa_cast_host::audio::install();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    kasa_remote::screen_mirror::install(kasa_cast_host::jpeg_source());
}

/// 그 기기를 네이티브 칸으로 볼 수 있나 — 이 판이 보고, 그 기기가 보내는 판이고, 카사넷 직통·믿는 중계일 때.
pub(crate) fn can_view(base: &str) -> bool {
    cfg!(any(target_os = "macos", windows))
        && kasa_device::kasanet::screen_capable(base)
        && kasa_device::kasanet::screen_path(base).is_some_and(|(p, _)| p != "ssh")
}

/// 그 기기(base) 화면에 붙는 길 — 이 앱 카사넷 노드로, 그 기기 화면 서비스가 있으면 서비스로. 그 기기가 화면 스트림을 모르는
/// 판이거나 카사넷 직통이 아니면 까닭만 들고 연다.
fn start(base: &str) -> kasa_castview::Start {
    if kasa_device::kasanet::screen_capable(base) {
        let base = base.to_string();
        return kasa_castview::Start::Connect(Box::new(move |config, on| kasa_remote::screen_stream::watch(base, config, on)));
    }
    kasa_castview::Start::Wait(match kasa_device::kasanet::path(base) {
        Some((p, _)) if p != "ssh" => kasa_castview::OLD_SENDER.into(),
        _ => kasa_castview::WAITING_PATH.into(),
    })
}

/// 검증 리그는 받은 소리를 장치로 내되 소리 없음으로 낸다(`kasa_castview::ViewAudio`).
fn silent_audio() -> bool {
    crate::verification_run() && std::env::var_os("KASATERM_TEST_SCREEN_AUDIO_SILENT").is_some()
}

/// Caps Lock 을 AppKit 에서 지켜본다 — 맥에서 Windows 를 조작 중이면 그 기기 한/영(`kasa_castview::watch_caps_lock`).
#[cfg(target_os = "macos")]
pub(crate) fn watch_caps_lock(proxy: &winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    let proxy = proxy.clone();
    kasa_castview::watch_caps_lock(move || drop(proxy.send_event(crate::UserEvent::ScreenCapsLock)));
}
