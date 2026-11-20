//! 화면 칸이 이 기기 소리를 받아 가는 동안 이 기기 스피커를 끈다 — 소리는 보는 쪽에서만 난다(Parsec 처럼). 뜨기는 끈
//! 스피커와 상관없다: Windows 루프백은 끝점 음소거 앞에서 뜨고, macOS SCK 는 앱 소리를 출력 장치 앞에서 뜬다
//! (`docs/screen-stream.md` 「원본 스피커 끄기」).
//!
//! 이 앱이 끈 것만 되켠다 — 원래 꺼져 있었으면 손대지 않고, 보는 동안 사람이 다시 켰으면 그대로 둔다. 앱이 죽어 못
//! 되켰으면 다음 실행이 표(`stream-muted`)를 보고 되켠다.

#[cfg(target_os = "macos")]
use mac as os;
#[cfg(windows)]
use win as os;

/// 지금 이 앱이 끈 장치 — 뜨기를 멈출 때·앱을 끌 때 되켠다. 앱 끄기는 정적 허브를 안 놓아서 끝날 때 따로 부른다.
static ACTIVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 끈 스피커 — 놓으면 되켠다.
pub(super) struct Muted;

impl Drop for Muted {
    fn drop(&mut self) {
        restore();
    }
}

/// 기본 출력 장치를 끈다. 이미 꺼져 있거나 끌 수 없는 장치(음소거가 없는 HDMI 등)면 None — 그때는 두 기기에서 다 난다.
pub(super) fn mute() -> Option<Muted> {
    // 검증 리그는 사람의 스피커를 끄지 않는다(같은 기기에 사람이 앉아 있다).
    if crate::verification_run() && std::env::var_os("KASATERM_TEST_SCREEN_SOURCE_MUTE").is_none() {
        return None;
    }
    if !crate::source_mute_on() {
        return None;
    }
    let device = os::default_output()?;
    if os::muted(&device)? {
        return None;
    }
    let path = marker()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, &device).ok()?;
    if !os::set(&device, true) {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(device);
    Some(Muted)
}

/// 끈 스피커를 되켠다. 보는 동안 사람이 이미 켰으면 그대로 둔다.
pub fn restore() {
    let Some(device) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    if os::muted(&device) == Some(true) {
        os::set(&device, false);
    }
    let _ = marker().map(std::fs::remove_file);
}

/// 부팅 때 — 지난 실행이 끈 채 죽었으면 되켠다.
pub(super) fn restore_after_crash() {
    let Some(path) = marker() else { return };
    let Ok(device) = std::fs::read_to_string(&path) else { return };
    let _ = std::fs::remove_file(&path);
    if os::muted(&device) == Some(true) {
        os::set(&device, false);
    }
}

fn marker() -> Option<std::path::PathBuf> {
    Some(crate::state_dir()?.join("stream-muted"))
}

#[cfg(windows)]
mod win {
    use windows::core::HSTRING;
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};

    fn devices() -> Option<IMMDeviceEnumerator> {
        crate::win::com();
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok()
    }

    /// 끝점 ID — 기기를 다시 켜도 같은 값이라 죽은 뒤 되켜기에 쓴다.
    pub(super) fn default_output() -> Option<String> {
        unsafe {
            let device = devices()?.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            let id = device.GetId().ok()?;
            let text = id.to_string().ok();
            CoTaskMemFree(Some(id.0 as *const _));
            text
        }
    }

    fn volume(device: &str) -> Option<IAudioEndpointVolume> {
        unsafe { devices()?.GetDevice(&HSTRING::from(device)).ok()?.Activate(CLSCTX_ALL, None).ok() }
    }

    pub(super) fn muted(device: &str) -> Option<bool> {
        unsafe { volume(device)?.GetMute() }.ok().map(|b| b.as_bool())
    }

    pub(super) fn set(device: &str, on: bool) -> bool {
        volume(device).is_some_and(|v| unsafe { v.SetMute(on, std::ptr::null()) }.is_ok())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const SYSTEM: u32 = 1;
    const DEFAULT_OUTPUT: u32 = u32::from_be_bytes(*b"dOut");
    const MUTE: u32 = u32::from_be_bytes(*b"mute");
    const GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const OUTPUT: u32 = u32::from_be_bytes(*b"outp");
    const MAIN: u32 = 0;

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            id: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: *mut u32,
            data: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            id: u32,
            address: *const Address,
            qualifier_size: u32,
            qualifier: *const c_void,
            size: u32,
            data: *const c_void,
        ) -> i32;
    }

    fn get(id: u32, address: &Address) -> Option<u32> {
        let mut value = 0u32;
        let mut size = 4u32;
        let status = unsafe {
            AudioObjectGetPropertyData(id, address, 0, std::ptr::null(), &mut size, (&mut value as *mut u32).cast())
        };
        (status == 0).then_some(value)
    }

    /// 장치 번호 — 이 부팅 안에서만 같다. 다시 켠 뒤 없는 번호면 되켜기가 조용히 실패한다.
    pub(super) fn default_output() -> Option<String> {
        get(SYSTEM, &Address { selector: DEFAULT_OUTPUT, scope: GLOBAL, element: MAIN }).map(|id| id.to_string())
    }

    pub(super) fn muted(device: &str) -> Option<bool> {
        get(device.parse().ok()?, &Address { selector: MUTE, scope: OUTPUT, element: MAIN }).map(|m| m != 0)
    }

    pub(super) fn set(device: &str, on: bool) -> bool {
        let Ok(id) = device.parse::<u32>() else { return false };
        let value = u32::from(on);
        let address = Address { selector: MUTE, scope: OUTPUT, element: MAIN };
        unsafe { AudioObjectSetPropertyData(id, &address, 0, std::ptr::null(), 4, (&value as *const u32).cast()) == 0 }
    }
}
