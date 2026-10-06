//! 터미널 칸을 그리는 엔진. 본판 카사텀과 새 카사라이트가 함께 쓴다.
//!
//! 학생·보드·계정·웹뷰 같은 본판 기능은 모른다. 색은 호스트가 [`palette::Palette`] 로, 글꼴은
//! 경로나 바이트로 넘긴다 — 크레이트 안에 큰 자산을 두지 않는다(cargo git 의존은 LFS 를 안 푼다).

pub mod cursor;
pub mod fonts;
pub mod geometry;
pub mod macos;
pub mod palette;
pub mod renderer;

pub use cursor::{cursor_primitives, CursorPrimitives, CursorQuad, CursorShape};
pub use geometry::{block_rects, box_line_rects};
pub use palette::{adapt_to_viewer, Palette, SourcePalette};
pub use renderer::{GridFonts, GridRenderer, PaneSlot};

/// 한 행 안의 셀 구간 `col_start..col_end`(끝 미포함). 칸 위에 밑줄을 긋는 링크 범위에 쓴다.
#[derive(Debug, Clone)]
pub struct CellSpan {
    pub row: u16,
    pub col_start: u16,
    pub col_end: u16,
}
