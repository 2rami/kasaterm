//! 거울 칸을 어떤 보기로 그리는가 — 한 곳의 판정. 정본 규칙은 `docs/mirror-render.md`.
//!
//! 거울은 원본 PTY 크기를 바꾸지 않는다. 원본 격자를 그대로 비추는 `Grid` 는 판정을
//! 못 하는 옛 원본에만 남는다 — 그때만 옛 규칙(만지면 크기 빌리기)이 산다.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorKind {
    /// Claude·코덱스 — 기록(+연결 mod)으로 대화형(`chat_view`).
    Chat,
    /// 셸 — OSC 133 명령 묶음(`shell_view`). 셸 통합이 없거나 전체 화면 프로그램에
    /// 들어간 동안은 원본 격자를 칸에 맞춰 줄여 보기만 한다.
    Shell,
    /// 원본 격자 그대로(옛 원본·판정 모름).
    Grid,
}

/// 대화형 기본을 다시 살피는 간격 — 학생이 거울 칸에서 막 뜬 것을 이만큼 안에 잡는다.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

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

    /// 보이는 거울 Claude·코덱스 칸은 대화로 연다 — 사람이 「터미널로 보기」로 돌려 둔 칸은 그대로 둔다.
    pub(crate) fn open_mirror_chats(&mut self) {
        let now = Instant::now();
        if self.chat_view.mirror_checked.is_some_and(|t| now.duration_since(t) < CHECK_EVERY) {
            return;
        }
        self.chat_view.mirror_checked = Some(now);
        let visible = self.visible_pane_ids();
        let want: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            visible
                .into_iter()
                .filter(|id| ws.panes.contains_key(id))
                .filter(|id| !self.chat_view_on(id) && !self.chat_view.declined.contains(id))
                .filter(|id| self.mirror_kind(&ws.active_tab_pid(id)) == MirrorKind::Chat && self.pane_can_chat(&ws, id))
                .collect()
        };
        for id in want {
            self.open_chat_view_quietly(&id);
        }
    }
}
