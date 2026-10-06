//! tmux control-mode (-C) bridge. GUI-agnostic.
//!
//! Spawn with [`session::TmuxSession::start`] and consume `events` /
//! `screens` channels from your UI thread.
//!
//! 화면 낱말(`screen`·`reflow`·`layout`)은 `kasa-screen` 으로 옮겼다. 옛 경로
//! (`kasa_bridge::screen::…`)를 그대로 쓰도록 모듈째 재수출한다.

pub mod event;
pub mod session;
mod vt;

pub use kasa_screen::{layout, reflow, screen};

pub use event::{parse_line, TmuxEvent};
pub use layout::{parse_layout, Layout};
pub use screen::{Cell, Color, Row, ScreenUpdate};
pub use session::{StartOptions, TmuxSession};
