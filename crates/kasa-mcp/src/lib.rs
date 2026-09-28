//! kasaterm 호스트의 HTTP 서버 — 보드·원격 방·모바일·웹 pane 이 이 한 포트로 붙는다.
//! 예전에는 pane 조작을 MCP 도구로도 내보냈지만 `kasaterm-cli` 와 완전히 겹쳐 걷었다.

pub mod character;
pub mod board_service;
pub mod changes;
pub mod browser_route;
pub mod browser_target;
pub mod codexhome;
pub mod dispatch;
pub mod git;
pub mod git_panel;
pub mod gridwire;
pub mod visual;
mod http;
pub mod persona;
mod proxy;
mod register;
pub mod relayconf;
pub mod relay;
pub mod feedback;
pub mod feedback_client;
pub mod relay_auth;
pub mod device_auth;
pub mod account_sync;
pub mod remote;
pub mod remote_restore;
pub mod surface_keys;
pub mod tell_service;
pub mod nacho_service;
mod nacho_relay;
#[cfg(unix)]
pub mod adopt;
pub mod layout_feed;
pub mod layout_watch;
pub mod machines;
pub mod mobile;
pub mod notes;
pub mod push;
pub mod uplink;
pub mod gateway;
pub mod remoteboard;
pub mod reposync;
mod resume_visibility;
pub mod standalone;
pub mod team;
pub mod tunnel;
pub mod quicktunnel;
pub use http::{
    claude_bin, pane_tasks_snapshot, remote_token, schedule_add, schedule_delete,
    schedule_snapshot, schedule_toggle, session_token, spawn_http_server,
    spawn_http_server_opts, PaneTaskView, ScheduleItem,
};
pub use http::push_viewer_control;
pub use nacho_relay::app_target as nacho_app_target;
pub use register::unregister_clients;

/// `Command` with the console window suppressed on Windows. kasaterm is a GUI
/// (non-console) process, so spawning a console program (git, etc.) flashes a
/// fresh console window each call — and a polled spawn flashes it on a loop.
/// CREATE_NO_WINDOW keeps it hidden. No-op on other platforms.
pub(crate) fn no_window_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    // `mut` 는 아래 windows 블록이 쓴다 — 떼면 그쪽 빌드가 깨지므로
    // 다른 플랫폼에서만 나는 경고를 끈다.
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}
