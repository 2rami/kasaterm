//! 화면 스트림의 OS 부분 — 화면 뜨기·하드웨어 압축과 풀기, 시스템 소리 뜨기·틀기·원본 스피커 끄기, 조작 넣기, JPEG 거울 한 장
//! (`docs/screen-stream.md`·`docs/screen-mirror.md`). 계약과 흐름은 `kasa-cast` 다.
//!
//! 앱과 앱 밖 화면 서비스(`docs/screen-service-split.md`)가 같은 코드를 쓴다 — 그래서 앱 설정·소켓·창을 모르고, 상태 파일
//! 자리는 부르는 쪽이 [`set_state_dir`] 로 준다.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

#[cfg(any(target_os = "macos", windows))]
pub mod audio;
#[cfg(target_os = "macos")]
mod audio_mac;
#[cfg(windows)]
mod audio_win;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(target_os = "macos")]
pub mod codec;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod scroll_probe;
#[cfg(any(target_os = "macos", windows))]
mod source_mute;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
mod win_agent;
#[cfg(windows)]
pub mod win_view;
#[cfg_attr(not(windows), allow(dead_code))]
mod annexb;
pub mod input;
mod jpeg;
#[cfg(unix)]
pub mod local;
mod wire;

/// 보는 칸이 쓰는 풀기 — 맥은 VideoToolbox(IOSurface), Windows 는 Media Foundation(D3D11 공유 텍스처).
#[cfg(target_os = "macos")]
pub use codec::{Picture, VideoDecoder};
#[cfg(target_os = "macos")]
pub use codec::zero_copy_result;
#[cfg(windows)]
pub use win_view::{zero_copy_result, Picture, VideoDecoder};

#[cfg(any(target_os = "macos", windows))]
pub use source_mute::restore as restore_speakers;

static STATE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 스피커를 끈 표(`stream-muted`)를 둘 폴더 — 앱은 설정 폴더, 화면 서비스는 제 상태 폴더. 부팅 때 한 번.
pub fn set_state_dir(dir: PathBuf) {
    let _ = STATE_DIR.set(dir);
}

fn state_dir() -> Option<PathBuf> {
    STATE_DIR.get().cloned()
}

/// 검증 실행인가 — 앱 리그(`KASATERM_WINDOW_SIZE`·`_POS`)와 창 없는 화면 서비스 리그(`KASATERM_SCREEN_VERIFY`).
/// 검증 실행에서만 사람 화면을 건드리지 않는 시험 길(스크롤 탐침·조작 기록·소리 없음)이 켜진다.
pub fn verification_run() -> bool {
    static RUN: OnceLock<bool> = OnceLock::new();
    *RUN.get_or_init(|| {
        ["KASATERM_WINDOW_SIZE", "KASATERM_WINDOW_POS", "KASATERM_SCREEN_VERIFY"].iter().any(|k| std::env::var_os(k).is_some())
    })
}

/// 이 OS 의 화면 압축 보내기. 못 보내는 OS 면 None.
pub fn capture() -> Option<Arc<dyn kasa_cast::Capture>> {
    #[cfg(target_os = "macos")]
    return Some(Arc::new(host::Host));
    #[cfg(windows)]
    return Some(Arc::new(win::Host));
    #[allow(unreachable_code)]
    None
}

/// 기기 화면 거울(JPEG)의 화면 뜨기.
pub fn jpeg_source() -> Arc<dyn kasa_cast::mirror::Source> {
    Arc::new(jpeg::Screen)
}

/// 호스트 시계(초) — 장의 시각과 같은 시계다. 맥은 CMClock 호스트 시계, Windows 는 QPC(다른 프로세스와 같은 기점).
#[cfg(target_os = "macos")]
pub fn host_now() -> f64 {
    unsafe { objc2_core_media::CMClock::host_time_clock().time().seconds() }
}

#[cfg(windows)]
pub fn host_now() -> f64 {
    win::now()
}

/// 그 기기 화면 목록(압축 정보 `displays`) — 보는 쪽이 화면 고르기를 세운다. 화면 거울과 같은 번호다.
#[cfg(target_os = "macos")]
fn displays_json() -> serde_json::Value {
    serde_json::Value::Array(
        jpeg::displays()
            .iter()
            .map(|d| serde_json::json!({ "id": d.id, "width": d.width, "height": d.height, "main": d.main }))
            .collect(),
    )
}

/// 원본 스피커 끄기를 쓸지 — 화면 서비스 설정(`source_mute`). 끄면 소리를 떠도 이 기기 스피커를 그대로 둔다.
static SOURCE_MUTE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_source_mute(on: bool) {
    SOURCE_MUTE.store(on, std::sync::atomic::Ordering::Relaxed);
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn source_mute_on() -> bool {
    SOURCE_MUTE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Windows 화면 서비스(`kasaterm-screen --screen-service`) 쪽 — 세션 0 에서 돌아 화면·소리는 콘솔 세션 도우미에서 받는다.
#[cfg(windows)]
pub mod service {
    pub use crate::win_agent::{configured_exe, connect_ctl, console_has_user, restart_service, run_with, state_dir};

    /// 도우미가 뜨는 시스템 소리 — 소리 허브(`kasa_cast::audio::install`)에 꽂는다.
    pub fn audio() -> std::sync::Arc<dyn kasa_cast::audio::Source> {
        std::sync::Arc::new(crate::win_agent::AgentAudio)
    }
}
