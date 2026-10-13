//! 거울 칸이 무엇인가 — 한 곳의 판정. 정본 규칙은 `docs/mirror-render.md`.
//!
//! 거울은 처음엔 터미널로 보인다. 대화(`chat_view`)·명령 묶음(`shell_view`)은 사람이 ⋮ 에서
//! 켤 때만 — 이 판정은 그때 무엇을 켤 수 있나와 크기를 빌리나를 가른다.
//! 거울은 원본 PTY 크기를 바꾸지 않는다. 원본 격자를 그대로 비추는 `Grid` 는 판정을
//! 못 하는 옛 원본에만 남는다 — 그때만 옛 규칙(만지면 크기 빌리기)이 산다.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorKind {
    /// Claude·코덱스 — ⋮ 「대화로 보기」로 기록(+연결 mod) 대화형(`chat_view`)을 켤 수 있다.
    Chat,
    /// 셸 — ⋮ 「명령으로 보기」로 OSC 133 명령 묶음(`shell_view`)을 켤 수 있다. 셸 통합이 없거나
    /// 전체 화면 프로그램에 들어간 동안은 원본 격자를 칸에 맞춰 줄여 보기만 한다.
    Shell,
    /// 원본 격자 그대로(옛 원본·판정 모름).
    Grid,
}

impl App {
    /// `surface` 는 거울 링크가 걸린 탭 pid. 거울이 아니면 `Grid`.
    pub(crate) fn mirror_kind(&self, surface: &str) -> MirrorKind {
        if !kasa_mcp::remote::is_view_pane(surface) {
            return MirrorKind::Grid;
        }
        match kasa_mcp::remote::cached_agent_running(surface) {
            Some(true) => MirrorKind::Chat,
            Some(false) if !self.shell_view.unsupported(surface) => MirrorKind::Shell,
            _ => MirrorKind::Grid,
        }
    }
}
