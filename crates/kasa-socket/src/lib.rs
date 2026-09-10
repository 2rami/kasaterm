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
pub mod peers;
pub mod protocol;
pub mod server;
pub mod sessions;
pub mod transport;
pub mod transfer;

pub use backend::{Backend, SplitDirection};
pub use protocol::{ErrorObj, Request, Response};
pub use server::Server;

/// 홈 디렉토리 — HOME(unix·Git bash) → USERPROFILE(Windows GUI 프로세스는 HOME
/// 미설정) 순. 둘 다 없으면 None — 호출부가 빈 PathBuf 로 폴백하면 종전 동작과 동일.
pub fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
        .map(std::path::PathBuf::from)
}

/// collab 마커·메시지 루트. unix 는 `/tmp/kasaterm-collab` 리터럴 유지 — sh 훅·
/// statusline 등 스크립트가 같은 리터럴을 참조한다. Windows 는 `%TEMP%` 기준 —
/// Git bash 가 `/tmp` 를 `%TEMP%` 로 마운트하므로 스크립트와 같은 디렉토리로 만난다.
pub fn collab_root() -> std::path::PathBuf {
    if let Some(root) = isolated_collab_root() { return root; }
    if cfg!(windows) {
        std::env::temp_dir().join("kasaterm-collab")
    } else {
        std::path::PathBuf::from("/tmp/kasaterm-collab")
    }
}

/// Native verification apps must not write, sweep or unlink the production
/// character registry, even when their TMPDIR/socket is different.
pub fn isolated_collab_root() -> Option<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("KASATERM_COLLAB_ROOT").filter(|v| !v.is_empty()) {
        return Some(root.into());
    }
    let verification = std::env::var_os("KASATERM_WINDOW_SIZE").is_some()
        || std::env::var_os("KASATERM_WINDOW_POS").is_some();
    verification_collab_root(verification, std::env::var_os("KASATERM_SESSION_FILE").as_deref(), std::process::id())
}

fn verification_collab_root(verification: bool, session: Option<&std::ffi::OsStr>, pid: u32) -> Option<std::path::PathBuf> {
    let session = session.filter(|v| verification && !v.is_empty())?;
    Some(std::path::Path::new(session).parent()?.join(format!("kasaterm-collab-verify-{pid}")))
}

/// pane↔sid bind 마커(`kasaterm-bound-<safe id>`). `collab_root` 과 같은 이유로
/// 플랫폼마다 갈린다 — 쓰는 쪽은 sh 훅(`kasaterm-bind-transcript.sh`)이라 리터럴
/// `/tmp` 를 쓰고, Windows 의 Git bash 에선 그게 `%TEMP%` 다. 지우는 쪽(GUI)이
/// 리터럴을 그대로 따라 하면 Windows 에서 영영 못 지운다.
pub fn bound_marker_path(safe_id: &str) -> std::path::PathBuf {
    let name = format!("kasaterm-bound-{safe_id}");
    if let Some(root) = isolated_collab_root() { return root.join(name); }
    if cfg!(windows) {
        std::env::temp_dir().join(name)
    } else {
        std::path::PathBuf::from("/tmp").join(name)
    }
}

#[cfg(test)]
mod collab_isolation_tests {
    use super::*;

    #[test]
    fn verification_registry_never_shares_production_or_another_probe() {
        let session = Some(std::ffi::OsStr::new("/probe/session.json"));
        let one = verification_collab_root(true, session, 101).unwrap();
        assert_eq!(one, std::path::PathBuf::from("/probe/kasaterm-collab-verify-101"));
        assert_ne!(one, verification_collab_root(true, session, 102).unwrap());
        assert!(verification_collab_root(false, session, 101).is_none());
        assert!(verification_collab_root(true, None, 101).is_none());
    }
}
