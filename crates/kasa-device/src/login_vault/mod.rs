//! 웹 로그인 동기화 — 카사 키체인의 `web:<host>` 로그인을 같은 계정의 기기끼리 맞춘다(docs/login-vault-sync.md).
//!
//! 관문에는 잠긴 덩어리와, 열쇠를 가진 기기가 맡긴 금고 열쇠가 간다. 같은 계정으로 로그인한 새 기기는 그 열쇠를 받아
//! 로그인만으로 맞추기 시작한다. 브라우저는 지금처럼 로컬 키체인만 읽는다 — 맞추는 일은 이 모듈이 한다.
//! 로그에는 개수만 남긴다. 사이트·아이디·비밀번호는 어디에도 찍지 않는다.

pub mod crypto;
pub mod engine;
pub mod http;
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

pub use service::{allow_deletions, create_vault, poke, spawn, status};
pub(crate) use service::key_confirmed;

#[cfg(test)]
mod tests;
