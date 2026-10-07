//! 카사텀 협업 호스트 — 보드 수집기(`board_service`), tell 장부(`tell_service`), 칸·방 쓰기(`act_service`), 칸 열쇠
//! (`surface_keys`), 기계 id(`identity`). 본판 GUI 와 `kasa tui` 서버가 같이 쓴다.
//!
//! 기기 명부·관문·원격 칸처럼 호스트마다 다른 것은 [`env::CollabEnv`] 로 받는다. 다른 기계와
//! 주고받는 HTTP(판 당겨 오기·원격 tell)는 feature `net` 이다.

pub mod act_service;
pub mod board_service;
pub mod delivery;
pub mod env;
pub mod hooks;
pub mod identity;
pub mod surface_keys;
pub mod tell_service;
