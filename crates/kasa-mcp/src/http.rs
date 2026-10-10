//! Streamable-HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.
//!
//! 자식 모듈은 기능으로 나뉜다. `router` 가 서버를 띄우고 모듈마다의 `routes()` 를 한 표로 합친 뒤
//! 공통 레이어(`auth::origin_guard_mw` → `auth::mobile_prefix_mw`)를 두른다 — 새 라우트도 그 레이어를 저절로 탄다.
//! `auth`(토큰·Origin·폰 주소) · `socket`(`/term/ws`) · `term_assets`·`term_api`·`term_files`(웹 터미널) ·
//! `migrate`(이사) · `claude_account`(계정 슬롯·사용량) · `sessions`·`pane_read`·`panes`(세션·칸) ·
//! `collab`(보드·보내기·tell) · `schedule`·`tasks`(스케줄·디스패처·팀 작업) · `files` · `git_api` ·
//! `settings` · `arona_ui` · `host` · `phone` · `machine_proxy`(`/m/<기계>/…`).

use std::sync::Arc;
use kasa_socket::backend::{Backend, CharacterSave};
use axum::{
    body::Bytes,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path as AxPath, Query},
    http::{header, HeaderMap, Method},
    response::IntoResponse,
    routing::{any, get, post},
    Json,
};
use crate::git;

mod router;
mod auth;
mod phone;
mod machine_proxy;
mod socket;
mod term_assets;
mod term_api;
mod term_files;
mod migrate;
mod claude_account;
mod sessions;
mod pane_read;
mod panes;
mod collab;
mod schedule;
mod tasks;
mod files;
mod git_api;
mod settings;
mod arona_ui;
mod host;
#[cfg(test)]
mod test_support;

pub use auth::{remote_token, session_token};
pub use router::{spawn_http_server, spawn_http_server_opts};
pub use schedule::{ScheduleItem, schedule_add, schedule_delete, schedule_snapshot, schedule_toggle};
pub use sessions::{claude_agents_all, claude_bin};
pub use socket::push_viewer_control;
pub use tasks::{PaneTaskView, pane_tasks_snapshot};
pub(crate) use auth::{MobileAuth, guest_denied, is_remote_peer, origin_guard_mw, ws_origin_ok};
pub(crate) use claude_account::{
    SlotLogin, claude_keychain_service, claude_slot_login, keychain_user,
};
#[cfg(test)]
pub(crate) use collab::collab_tell_post;

/// Directory git commands run in: follow the active pane's shell cwd so the
/// panel tracks the user's terminal directory; fall back to the host cwd.
pub(crate) fn resolve_cwd(backend: &Arc<dyn Backend>) -> std::path::PathBuf {
    backend
        .active_cwd()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")))
}
