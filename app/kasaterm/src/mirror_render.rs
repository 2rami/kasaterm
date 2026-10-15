//! 거울 칸이 무엇이고 무엇으로 보이나 — 한 곳의 판정. 정본 규칙은 `docs/mirror-render.md`.
//!
//! 거울은 원본 크기가 어떻든 보는 칸에 맞게 보인다. Claude·코덱스는 대화(`chat_view`), 셸 통합이 있는 셸은
//! 명령 묶음(`shell_view`)이 처음 얼굴이다 — 둘 다 원본 PTY 를 안 건드리고 기록·구간을 이 칸 폭으로 다시
//! 짓는다. 사람이 ⋮ 에서 터미널로 돌린 칸은 칸마다 기억해 다시 열지 않는다. 기록으로 다시 그릴 수 없는
//! 칸(전체 화면 앱·셸 통합 없는 셸)만 보거나 만지는 동안 원본 격자를 이 칸 크기로 잡는다(`mirror_follow.rs`).
use super::*;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorKind {
    /// Claude·코덱스 — 기록(+연결 mod)으로 대화형(`chat_view`).
    Chat,
    /// 셸 — OSC 133 명령 묶음(`shell_view`). 셸 통합이 없거나 전체 화면 프로그램에 들어간 동안은
    /// 원본 격자를 보이고, 그동안 원본 크기를 이 칸으로 잡는다.
    Shell,
    /// 원본 격자 그대로(명령 묶음을 모르는 옛 원본·판정 모름).
    Grid,
}

/// 거울 칸들을 다시 살피는 간격 — 학생이 거울 칸에서 막 뜬 것을 이만큼 안에 잡는다.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct MirrorViews {
    checked: Option<Instant>,
    hold: crate::mirror_follow::ViewHold,
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

    /// 이 거울을 다시 그릴 수 없나 — 그럴 때만 보는 동안 원본 격자를 이 칸 크기로 잡는다.
    pub(crate) fn mirror_needs_source(&self, ws: &Workspace, id: &str, tab: &str) -> bool {
        let alt = ws.panes.get(id).and_then(|p| p.term()).is_some_and(|t| t.alt_screen);
        match self.mirror_kind(tab) {
            // 대화는 기록으로 짓는다 — 사람이 터미널로 돌린 풀스크린 학생만 남는다.
            MirrorKind::Chat => alt && self.chat_view.declined.contains(id),
            MirrorKind::Shell => alt || self.shell_view_lacks_integration(id),
            // 명령 묶음을 모르는 옛 원본의 셸. 판정을 아직 모르는 칸은 기다린다.
            MirrorKind::Grid => kasa_mcp::remote::cached_agent_running(tab).is_some(),
        }
    }

    /// 보이는 거울 칸을 처음 얼굴로 열고, 다시 그릴 수 없는 칸은 보는 동안 원본 크기를 잡는다.
    pub(crate) fn pump_mirror_views(&mut self) {
        let now = Instant::now();
        if self.mirror_views.checked.is_some_and(|t| now.duration_since(t) < CHECK_EVERY) {
            return;
        }
        self.mirror_views.checked = Some(now);
        let visible = self.visible_pane_ids();
        let mut chats = Vec::new();
        let mut shells = Vec::new();
        {
            let ws = self.ws.lock().unwrap();
            for id in visible.iter().filter(|id| ws.panes.contains_key(*id)) {
                match self.mirror_kind(&ws.active_tab_pid(id)) {
                    MirrorKind::Chat if !self.chat_view_on(id) && !self.chat_view.declined.contains(id) && self.pane_can_chat(&ws, id) => {
                        chats.push(id.clone())
                    }
                    MirrorKind::Shell if !self.shell_view_on(id) && !self.shell_view.declined.contains(id) && self.pane_can_blocks(&ws, id) => {
                        shells.push(id.clone())
                    }
                    _ => {}
                }
            }
        }
        for id in chats {
            self.open_chat_view_quietly(&id);
        }
        for id in shells {
            self.open_shell_view_quietly(&id);
        }
        let (seen, want) = {
            let ws = self.ws.lock().unwrap();
            let mut seen = HashSet::new();
            let mut want = HashSet::new();
            for id in visible.iter().filter(|id| ws.panes.contains_key(*id)) {
                let tab = ws.active_tab_pid(id);
                if !kasa_mcp::remote::is_view_pane(&tab) {
                    continue;
                }
                if self.mirror_needs_source(&ws, id, &tab) {
                    want.insert(tab.clone());
                }
                seen.insert(tab);
            }
            (seen, want)
        };
        let focused = self.target_surface();
        self.mirror_views.hold.step(&seen, &want, focused.as_deref(), now);
    }
}
