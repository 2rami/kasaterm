//! Direct-PTY backend for kasaterm. Spawns a shell into a real PTY via
//! `portable-pty` (so the same code path lights up macOS/Linux's
//! BSD-style PTY and Windows' ConPTY), feeds the byte stream through
//! `alacritty_terminal`'s VT processor, and emits the same
//! `ScreenUpdate` shape tmux-bridge produces so the renderer doesn't
//! care which backend is driving it.
//!
//! Phase C scope. Single-PTY MVP — multi-pane multiplexing is the
//! follow-up. The cmux-style socket API can already split panes when
//! the tmux backend is running; once Phase C is past MVP we add an
//! in-process multiplexer (`Workspace` of `PtySession`s).

pub mod layout;
mod state;

pub use crossbeam_channel::Receiver as ScreenReceiver;
pub use layout::{Divider, PtyLayout, SplitDir};
pub use state::{
    live_sessions, lookup_session, process_cmdline, process_env_var, process_table,
    register_session, CommandBlock, PtyOptions, PtySession,
};
