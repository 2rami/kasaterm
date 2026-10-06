//! 터미널 화면 낱말. 백엔드(kasa-pty·옛 tmux 브리지)가 내고 렌더러(본판 GUI·
//! 카사라이트·`kasa tui`)가 받는 셀·행·화면 갱신, ANSI 직렬화, 리플로우, 칸 배치.
//! GUI·PTY 어느 쪽에도 기대지 않는다.

pub mod layout;
pub mod reflow;
pub mod screen;

pub use layout::{parse_layout, Layout};
pub use screen::{Cell, Color, Row, ScreenUpdate};
