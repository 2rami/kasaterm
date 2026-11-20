//! 화면 스트림 — 다른 기기 화면을 하드웨어 영상 압축으로 카사텀 칸 안에 본다. 계약·실측은 `docs/screen-stream.md`.
//! 틀과 흐름은 `kasa_cast`, OS 부분(뜨기·압축·풀기·소리·조작 넣기)은 `kasa_cast_host`, 여기는 보는 칸이다. 보는 칸(`view.rs`)은
//! 맥·Windows 가 같고 풀기만 `platform` 으로 갈린다.

#[cfg(any(target_os = "macos", windows))]
mod audio;
#[cfg(any(target_os = "macos", windows))]
pub(crate) use kasa_cast_host::restore_speakers;
#[cfg(any(target_os = "macos", windows))]
mod view;

#[cfg(any(target_os = "macos", windows))]
use kasa_cast_host as platform;
mod input;
#[cfg(target_os = "macos")]
pub(crate) use input::watch_caps_lock;
mod pane;
mod window;

pub(crate) use pane::paint;
pub(crate) use window::ScreenWindow;

/// 화면 보기가 그려지는 창 — 본창의 칸이거나 꺼낸 화면 창(`window.rs`). 누름은 그 창에서 온 것만 받는다.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Surface {
    #[default]
    Main,
    Window(winit::window::WindowId),
}

#[cfg(any(target_os = "macos", windows))]
pub(crate) use view::ScreenView;

/// 부팅 때 한 번 — 카사넷 화면 입구와 이 기기 소리 허브를 연다. 화면 기록 권한은 붙는 때 따진다.
#[cfg(any(target_os = "macos", windows))]
pub(crate) fn install() {
    if let Some(capture) = kasa_cast_host::capture() {
        kasa_remote::screen_stream::install(capture);
    }
    kasa_cast_host::audio::install();
}

/// `kasaterm.exe --screen-service`·`--screen-agent` 면 Windows 화면 도우미로 돌고 참 — 앱 초기화 전에 부른다.
#[cfg(windows)]
pub(crate) fn run_helper(args: &[std::ffi::OsString]) -> bool {
    kasa_cast_host::run_helper(args)
}

/// 그 기기를 네이티브 칸으로 볼 수 있나 — 이 판이 보고, 그 기기가 보내는 판이고, 카사넷 직통·믿는 중계일 때.
pub(crate) fn can_view(base: &str) -> bool {
    cfg!(any(target_os = "macos", windows))
        && kasa_device::kasanet::screen_capable(base)
        && kasa_device::kasanet::path(base).is_some_and(|(p, _)| p != "ssh")
}

type Rect = (f32, f32, f32, f32);

/// 칸 위에 그린 자리(LOGICAL px) — 누름이 어디에 떨어졌는지 가른다.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    /// 장이 그려진 사각형(비율 맞춤·확대 뒤).
    pub image: Rect,
    /// `image` 에 담긴 장의 범위(u0·v0·u1·v1, 장 안 비율) — 확대하면 일부다.
    pub uv: Rect,
    /// 칸 본문 — 확대한 장이 펴지는 자리.
    pub slot: Rect,
    /// 장 크기(px).
    pub frame: (u32, u32),
    /// 「조작」 단추.
    pub toggle: Rect,
    /// 확대 배율 표시(누르면 맞춤으로). 맞춤이면 없다.
    pub zoom_chip: Option<Rect>,
    /// 「키」 단추(조작 중에만) — 이 기기 키보드로 못 보내는 키 메뉴를 연다.
    pub key_button: Option<Rect>,
    /// 「화면 2/3」 — 그 기기 화면이 둘 이상이면. 누르면 다음 화면으로.
    pub display_chip: Option<Rect>,
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
/// 확대 상한 — 4K 화면의 한 글자가 칸에서 손톱만 해지는 정도.
pub(crate) const MAX_ZOOM: f32 = 8.0;

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
/// 확대한 장을 칸에 펴는 자리 — (그릴 사각형, 거기 담길 장의 범위). `zoom` 1 은 비율 맞춤이고, 크면 그 축에서 칸을 채우며
/// `focus`(장 안 비율)를 가운데에 둔다. 장 끝을 넘어 비지 않게 가장자리에서 멈춘다.
pub(crate) fn zoomed(slot: Rect, frame: (u32, u32), zoom: f32, focus: (f64, f64)) -> (Rect, Rect) {
    let (fw, fh) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let scale = (slot.2 / fw).min(slot.3 / fh) * zoom.clamp(1.0, MAX_ZOOM);
    let (x, w, u0, u1) = zoom_axis(slot.0, slot.2, fw, scale, focus.0 as f32);
    let (y, h, v0, v1) = zoom_axis(slot.1, slot.3, fh, scale, focus.1 as f32);
    ((x, y, w, h), (u0, v0, u1, v1))
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
/// 한 축 — (그릴 자리, 길이, 범위 시작, 끝).
fn zoom_axis(pos: f32, len: f32, frame: f32, scale: f32, focus: f32) -> (f32, f32, f32, f32) {
    let shown = len / scale;
    if shown >= frame {
        let drawn = frame * scale;
        return (pos + (len - drawn) / 2.0, drawn, 0.0, 1.0);
    }
    let center = (focus * frame).clamp(shown / 2.0, frame - shown / 2.0);
    (pos, len, (center - shown / 2.0) / frame, (center + shown / 2.0) / frame)
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
/// `focus` 를 그 배율에서 실제로 보이는 가운데로 붙인다 — 끝에 붙은 채 더 민 만큼이 쌓여 되돌릴 때 헛돌지 않게.
pub(crate) fn clamp_focus(slot: Rect, frame: (u32, u32), zoom: f32, focus: (f64, f64)) -> (f64, f64) {
    let (_, uv) = zoomed(slot, frame, zoom, focus);
    (f64::from(uv.0 + uv.2) / 2.0, f64::from(uv.1 + uv.3) / 2.0)
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn contains((x, y, w, h): (f32, f32, f32, f32), (px, py): (f64, f64)) -> bool {
    let (px, py) = (px as f32, py as f32);
    px >= x && px < x + w && py >= y && py < y + h
}

#[cfg(not(any(target_os = "macos", windows)))]
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
    pub surface: Surface,
    pub closed_since: Option<std::time::Instant>,
}

#[cfg(not(any(target_os = "macos", windows)))]
#[allow(dead_code)]
impl ScreenView {
    pub(crate) fn open(_label: &str, _base: &str, _wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        None
    }
    pub(crate) fn new_surface(&mut self) -> Option<(*mut std::ffi::c_void, u64, u32, u32)> {
        None
    }
    pub(crate) fn current_surface(&mut self) -> Option<(*mut std::ffi::c_void, u64, u32, u32)> {
        None
    }
    pub(crate) fn closed(&self) -> bool {
        true
    }
    pub(crate) fn carry_status(&self, _previous: &str) {}
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

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: Rect = (10.0, 20.0, 800.0, 600.0);
    const ULTRAWIDE: (u32, u32) = (3840, 1600);

    #[test]
    fn zoom_one_is_the_contain_fit_and_shows_the_whole_frame() {
        let (rect, uv) = zoomed(SLOT, ULTRAWIDE, 1.0, (0.9, 0.1));
        assert_eq!(uv, (0.0, 0.0, 1.0, 1.0));
        assert_eq!(rect.2, 800.0);
        assert!((rect.3 - 333.333).abs() < 0.01);
        assert!((rect.1 - (20.0 + (600.0 - rect.3) / 2.0)).abs() < 0.01, "세로는 가운데");
    }

    #[test]
    fn zooming_fills_the_overflowing_axis_around_the_focus() {
        let (rect, uv) = zoomed(SLOT, ULTRAWIDE, 2.0, (0.5, 0.5));
        assert_eq!((rect.0, rect.2), (10.0, 800.0));
        assert!((uv.0 - 0.25).abs() < 1e-4 && (uv.2 - 0.75).abs() < 1e-4);
        // 세로도 2배면 667 로 칸(600)을 넘쳐 칸을 채운다.
        assert_eq!((rect.1, rect.3), (20.0, 600.0));
    }

    #[test]
    fn focus_stops_at_the_frame_edge() {
        let (_, uv) = zoomed(SLOT, ULTRAWIDE, 4.0, (0.0, 1.0));
        assert_eq!(uv.0, 0.0);
        assert!((uv.3 - 1.0).abs() < 1e-6);
        let back = clamp_focus(SLOT, ULTRAWIDE, 4.0, (-3.0, 9.0));
        assert!((back.0 - 0.125).abs() < 1e-4, "{back:?}");
        assert!(zoomed(SLOT, ULTRAWIDE, 99.0, (0.5, 0.5)).1 == zoomed(SLOT, ULTRAWIDE, MAX_ZOOM, (0.5, 0.5)).1);
    }
}
