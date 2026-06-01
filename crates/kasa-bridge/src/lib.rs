//! tmux control-mode (-C) bridge. GUI-agnostic.
//!
//! Spawn with [`session::TmuxSession::start`] and consume `events` /
//! `screens` channels from your UI thread.

pub mod event;
pub mod layout;
pub mod screen;
pub mod session;

pub use event::{parse_line, TmuxEvent};
pub use layout::{parse_layout, Layout};
pub use screen::{Cell, Color, Row, ScreenUpdate};
pub use session::{StartOptions, TmuxSession};
