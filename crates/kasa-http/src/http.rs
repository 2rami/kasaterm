//! Local HTTP serving glue. The host (kasaterm) is a synchronous
//! winit/wgpu app, so we own a small multi-thread tokio runtime on a
//! dedicated background thread and run axum there. The `Backend` is
//! channel-based and `Send + Sync`, so calling it from async handlers on
//! another thread is safe.
//!
//! 자식 모듈은 기능으로 나뉜다. `router` 가 서버를 띄우고 모듈마다의 `routes()` 를 한 표로 합친 뒤
//! 공통 레이어(`auth::origin_guard_mw` → `auth::mobile_prefix_mw`)를 두른다 — 새 라우트도 그 레이어를 저절로 탄다.
//! `kasa-device/access/auth`(토큰·Origin·폰 주소) · `socket`(`/term/ws`) · `term_assets`·`term_api`·`term_files`(웹 터미널) ·
//! `migrate`(이사) · `kasa-device/access/claude_account`(계정 슬롯·사용량) · `sessions`·`pane_read`·`panes`(세션·칸) ·
//! `collab`(보드·보내기·tell) · `kasa-students/http/{schedule,tasks}`(스케줄·디스패처·팀 작업) · `files` · `inline_media`(대화 소리·영상 원본) · `git_api` ·
//! `settings` · `arona_ui` · `host` · `kasa-device/access/phone` · `machine_proxy`(`/m/<기계>/…`) · `screen`(기기 화면 거울).

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
use kasa_git::git;
use kasa_socket::backend::resolve_cwd;
use kasa_device::access::{auth, phone, claude_account};
use kasa_students::http::{schedule, tasks};

mod router;
mod phone_browser;
mod machine_proxy;
mod screen;
mod app_control;
mod agent_plan;
mod socket;
mod term_assets;
mod term_api;
mod term_files;
mod migrate;
mod sessions;
mod pane_read;
mod panes;
mod collab;
mod files;
mod inline_media;
mod git_api;
mod settings;
mod arona_ui;
mod host;
mod shared_rooms;
mod replay;
#[cfg(test)]
use kasa_socket::test_support;

use auth::remote_token;
pub use router::{spawn_http_server, spawn_http_server_opts};
use kasa_agents::claude_bin;
pub use socket::push_viewer_control;
pub(crate) use auth::{MobileAuth, guest_denied, is_remote_peer, origin_guard_mw, ws_origin_ok};
#[cfg(test)]
pub(crate) use collab::collab_tell_post;

