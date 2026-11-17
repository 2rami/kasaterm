//! 북마크·탭 동기화 — 카사브라우저의 북마크를 같은 계정의 기기끼리 합치고, 열린 탭은 기기마다 보낸다
//! (kasabrowser `docs/browser-sync.md`). 열쇠는 로그인 금고 열쇠의 `browser-sync` 갈래라 따로 등록하지 않는다.
//! 관문에는 잠긴 덩어리만 가고, 브라우저와는 그 데이터 폴더의 로컬 창구(`sync.*`)로만 주고받는다.
//! 로그에는 개수만 남긴다. 주소·제목은 어디에도 찍지 않는다.

pub mod browser;
pub mod engine;
pub mod order;
pub mod record;
pub mod state;
pub mod tree;

#[cfg(test)]
mod tests;
