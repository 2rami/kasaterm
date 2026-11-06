use kasa_agents::claude_bin;
use axum::{extract::Query, http::{header, HeaderMap, Method}, response::IntoResponse, routing::get, Json};
pub mod auth;
pub mod phone;
pub mod claude_account;
pub use auth::{remote_token, session_token, MobileAuth, guest_denied, is_remote_peer, origin_guard_mw, ws_origin_ok};
pub use claude_account::{SlotLogin, claude_keychain_service, claude_slot_login, keychain_user, VerifiedClaudeIdentity, verified_claude_identity_for_token};
#[cfg(test)]
use kasa_socket::test_support;
