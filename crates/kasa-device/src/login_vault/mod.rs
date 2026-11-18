//! 웹 로그인 동기화 — 카사 키체인의 `web:<host>` 로그인을 같은 계정의 기기끼리 맞춘다(docs/login-vault-sync.md).
//!
//! 관문에는 잠긴 덩어리만 간다. 열쇠는 기기 키체인에만 있고, 새 기기는 폰 Face ID 승인을 받은 뒤 이미 열쇠를 가진
//! 기기가 직접 봉인해 넘긴다. 브라우저는 지금처럼 로컬 키체인만 읽는다 — 맞추는 일은 이 모듈이 한다.
//! 로그에는 개수만 남긴다. 사이트·아이디·비밀번호는 어디에도 찍지 않는다.

pub mod crypto;
pub mod engine;
pub mod http;
pub mod join;
pub mod record;
pub mod server;
mod service;
pub mod state;
pub mod store;
pub mod vault_key;
#[cfg(target_os = "macos")]
pub mod macos_store;
#[cfg(windows)]
pub mod windows_store;

pub use service::{allow_deletions, confirm_fingerprint, confirm_phone, create_vault, poke, spawn, start_join, status};
pub(crate) use service::key_confirmed;

#[cfg(test)]
mod tests;
