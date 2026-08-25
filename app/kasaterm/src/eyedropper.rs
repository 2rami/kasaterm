//! 화면 어디서나 색 집기 — 우리 창 밖, 다른 앱 위에서도.
//!
//! 픽셀을 우리가 읽지 않는다. macOS 가 주는 확대경 스포이드(`NSColorSampler`)를
//! 띄우고 시스템이 집어 준 색만 건네받으므로 **화면 기록 권한이 필요 없다** —
//! 직접 캡처해 읽는 길(`CGDisplayCreateImage`·`screencapture`)은 그 권한에 막힌다.
//!
//! 시스템이 색을 건네는 시점은 사용자가 클릭한 뒤라, 액션은 스포이드를 띄우기만
//! 하고 끝난다. 집힌 색은 여기 통에 담기고 GUI 틱이 꺼내 팔레트에 반영한다.

use std::sync::Mutex;

/// 집힌 색과 그것을 넣을 팔레트 칸. 콜백은 GUI 스레드로 돌아오지만 그 안에서는
/// `App` 을 만질 수 없어(클로저가 `&mut App` 을 못 든다) 여기 놓고 간다.
static PICKED: Mutex<Option<(usize, [u8; 3])>> = Mutex::new(None);

/// 집힌 색을 꺼낸다(있으면 비운다). GUI 틱이 매 프레임 부른다.
pub(crate) fn take_picked() -> Option<(usize, [u8; 3])> {
    PICKED.lock().ok().and_then(|mut g| g.take())
}

/// 이 OS 에서 화면 집기가 되는가. 안 되는 곳에서는 화면에 단추를 아예 안 낸다 —
/// 눌러야 안 된다는 걸 아는 단추보다 없는 편이 낫다.
pub(crate) const fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// 스포이드를 띄운다. 사용자가 화면 어디든 클릭하면 그 색이 `slot` 칸의 값으로
/// 담긴다. 취소하면 아무것도 담기지 않는다.
///
/// **메인 스레드에서 불러야 한다** — AppKit 규칙이고, 설정 액션은 이미 GUI
/// 스레드에서 처리되므로 그 경로로만 들어온다.
#[cfg(target_os = "macos")]
pub(crate) fn pick_screen_color(slot: usize) {
    use objc2_app_kit::{NSColor, NSColorSampler, NSColorSpace};

    let sampler = NSColorSampler::new();
    let handler = block2::RcBlock::new(move |color: *mut NSColor| {
        // 취소하면 nil 이 온다.
        if color.is_null() {
            return;
        }
        // 집힌 색은 그 화면의 색공간(P3 등)을 달고 온다. sRGB 로 옮기지 않으면
        // 같은 색을 찍어도 팔레트에 다른 hex 가 적힌다 — 이 앱의 팔레트는 sRGB
        // hex 가 정본이다.
        let c = unsafe { &*color };
        let Some(srgb) = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) else {
            return;
        };
        let ch = |f: f64| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
        let rgb = [
            ch(srgb.redComponent()),
            ch(srgb.greenComponent()),
            ch(srgb.blueComponent()),
        ];
        if let Ok(mut g) = PICKED.lock() {
            *g = Some((slot, rgb));
        }
    });
    // 세션이 끝날 때까지 sampler 를 시스템이 붙잡아 준다(NSColorSampler 문서) —
    // 그래서 여기서 떨어뜨려도 스포이드가 사라지지 않는다.
    unsafe { sampler.showSamplerWithSelectionHandler(&handler) };
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn pick_screen_color(_slot: usize) {}
