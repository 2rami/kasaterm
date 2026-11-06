//! 에이전트 하네스(Claude Code·Codex)에 대해 아는 것 — GUI 없이 돈다. 지금은 대화 기록
//! (transcript·rollout) 읽기다. 본판 GUI 와 `kasa tui` 가 같이 쓴다.

pub mod transcript;
pub mod codex_children;

pub mod runtime;
pub use runtime::{claude_agents_all, claude_bin};
