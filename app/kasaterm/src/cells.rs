//! 칸 색 — 테마 값을 엔진 팔레트(`kasa_gridview::Palette`)로 옮긴다. 색을 푸는 규칙은 엔진에 있고,
//! 여기는 본판 곳곳이 부르던 이름을 그대로 둔다.

pub use kasa_gridview::palette::{SourcePalette, SELECTION as ITERM_SELECTION};

/// 지금 테마로 채운 칸 팔레트. 테마는 실행 중에 바뀌므로 그릴 때마다 새로 뜬다(색 스무 개 남짓이라
/// 프레임마다 떠도 값이 싸다). 셀마다 부르지는 말 것.
pub fn palette() -> kasa_gridview::Palette {
    kasa_gridview::Palette {
        ansi16: std::array::from_fn(crate::theme::ansi16),
        fg: default_fg(),
        bg: default_bg(),
        cursor: crate::theme::cursor(),
        min_contrast: crate::theme::min_contrast(),
    }
}

/// 셀이 `Color::Default` 일 때의 전경.
#[inline]
pub fn default_fg() -> [u8; 4] {
    crate::theme::fg()
}
/// 터미널 본문 바탕 — 크롬과 본문이 한 팔레트를 쓰도록 테마 토큰 하나에서만 온다.
#[inline]
pub fn default_bg() -> [u8; 4] {
    crate::theme::pane_bg()
}
