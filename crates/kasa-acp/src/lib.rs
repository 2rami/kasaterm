//! ACP(Agent Client Protocol) 클라이언트.
//!
//! 카사텀은 학생의 상태(끝났나·권한을 기다리나·컨텍스트가 얼마나 찼나)를 Claude Code·Codex 의
//! TUI 화면을 읽어 짐작해 왔다. 사람 없이 도는 학생에게는 그 짐작이 증식·오판의 원인이었다.
//! 여기서는 어댑터(`@agentclientprotocol/claude-agent-acp`·`codex-acp`)를 자식 프로세스로 띄우고
//! stdio 위 줄 단위 JSON-RPC 로 주고받아, 같은 사실을 에이전트가 보낸 이벤트로 받는다.
//!
//! 에이전트 → 클라이언트 요청은 권한 요청만 받는다. 파일·터미널 capability 는 끈다 —
//! codex-acp 는 켜 줘도 명령을 자기가 직접 돌렸고(2026-11 실측), 카사텀이 대신 쓰면
//! 에이전트 쪽 샌드박스 판정과 어긋난다.

mod event;
mod launch;
mod rpc;
pub mod runner;

pub use event::{
    ConfigOption, Event, NewSession, PermissionKind, PermissionOption, PermissionRequest,
    PlanEntry, PromptOutcome, StopReason, ToolCall, ToolCallUpdate, ToolOutput, ToolStatus,
};
pub use launch::{Harness, LaunchSpec, codex_pane_env_config, find_program};
pub use rpc::{Connection, Error, Pending, RequestId};
