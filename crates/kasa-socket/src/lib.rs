//! cmux-compatible Unix-socket JSON-RPC server.
//!
//! This crate intentionally mirrors cmux's wire format (line-delimited
//! JSON over a Unix socket at `$CMUX_SOCKET_PATH` / `$KASATERM_SOCKET_PATH`)
//! so any agent that speaks the cmux protocol — currently Claude Code's
//! teammateMode proposal in anthropics/claude-code#36926 — can drive a
//! kasaterm session with no protocol shim. The cmux project's own CLI
//! (`cmux notify`, `cmux split`, etc.) also targets the same socket
//! contract.
//!
//! The crate stays renderer- and runtime-agnostic. It owns the listener
//! thread, the line-delimited JSON framing, and the request/response
//! routing. Concrete command behavior (split this pane, send these
//! bytes, list workspaces) lives behind the `Backend` trait — host apps
//! plug in whatever data source they have (tmux-bridge today,
//! portable-pty later) without the protocol layer caring.
//!
//! Frames are line-delimited JSON objects. Each request carries an `id`,
//! a `method` (e.g. `surface.split`), and a `params` object. Responses
//! echo the `id` and carry either `ok: true` with a `result` value or
//! `ok: false` with an `error` object. See `protocol.rs` for the exact
//! shapes.

pub mod backend;
pub mod methods;
pub mod protocol;
pub mod server;
pub mod sessions;
pub mod transport;

pub use backend::{Backend, SplitDirection};
pub use protocol::{ErrorObj, Request, Response};
pub use server::Server;

/// collab 마커·메시지 루트. unix 는 `/tmp/kasaterm-collab` 리터럴 유지 — sh 훅·
/// statusline 등 스크립트가 같은 리터럴을 참조한다. Windows 는 `%TEMP%` 기준 —
/// Git bash 가 `/tmp` 를 `%TEMP%` 로 마운트하므로 스크립트와 같은 디렉토리로 만난다.
pub fn collab_root() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::temp_dir().join("kasaterm-collab")
    } else {
        std::path::PathBuf::from("/tmp/kasaterm-collab")
    }
}
