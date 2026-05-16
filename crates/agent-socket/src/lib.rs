//! cmux-compatible Unix-socket JSON-RPC server.
//!
//! This crate intentionally mirrors cmux's wire format (line-delimited
//! JSON over a Unix socket at `$CMUX_SOCKET_PATH` / `$TMUXIFY_SOCKET_PATH`)
//! so any agent that speaks the cmux protocol — currently Claude Code's
//! teammateMode proposal in anthropics/claude-code#36926 — can drive a
//! tmuxify session with no protocol shim. The cmux project's own CLI
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

pub use backend::{Backend, SplitDirection};
pub use protocol::{ErrorObj, Request, Response};
pub use server::Server;
