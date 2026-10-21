//! 거울 칸이 무엇이고 무엇으로 보이나 — 한 곳의 판정. 정본 규칙은 `docs/mirror-render.md`.
//!
//! PC 거울은 원본 TUI 격자로 시작한다. 대화 보기는 사람이 골랐을 때만 다시 짓는다(셸은 늘 원본 격자).
//! 원본 격자를 보는 칸은 뷰어 크기로 원본 PTY 를 잡고, 안 보거나 재구성 보기를 켜면 놓는다.
use super::*;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorKind {
    /// Claude·코덱스 — 대화 보기를 선택할 수 있다.
    Chat,
    /// 원본 격자 그대로 — 셸·전체 화면 프로그램·판정 모름.
    Grid,
}

/// 거울 칸들을 다시 살피는 간격 — 학생이 거울 칸에서 막 뜬 것을 이만큼 안에 잡는다.
const CHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct MirrorViews {
    checked: Option<Instant>,
    hold: crate::mirror_follow::ViewHold,
}

fn needs_source(kind: MirrorKind, chat: bool, known: bool) -> bool {
    match kind {
        MirrorKind::Chat => !chat,
        MirrorKind::Grid => known,
    }
}

impl App {
    /// `surface` 는 거울 링크가 걸린 탭 pid. 거울이 아니면 `Grid`.
    pub(crate) fn mirror_kind(&self, surface: &str) -> MirrorKind {
        if !kasa_mcp::remote::is_view_pane(surface) {
            return MirrorKind::Grid;
        }
        match kasa_mcp::remote::cached_agent_running(surface) {
            Some(true) => MirrorKind::Chat,
            _ => MirrorKind::Grid,
        }
    }

    /// 이 거울을 다시 그릴 수 없나 — 그럴 때만 보는 동안 원본 격자를 이 칸 크기로 잡는다.
    pub(crate) fn mirror_needs_source(&self, id: &str, tab: &str) -> bool {
        needs_source(self.mirror_kind(tab), self.chat_view_on(id), kasa_mcp::remote::cached_agent_running(tab).is_some())
    }

    /// 원본 TUI를 보는 거울만 뷰어 크기를 빌려야 원본 앱이 그 폭으로 다시 그린다.
    pub(crate) fn pump_mirror_views(&mut self) {
        let now = Instant::now();
        if self.mirror_views.checked.is_some_and(|t| now.duration_since(t) < CHECK_EVERY) {
            return;
        }
        self.mirror_views.checked = Some(now);
        let visible = self.visible_pane_ids();
        let (seen, want) = {
            let ws = self.ws.lock().unwrap();
            let mut seen = HashSet::new();
            let mut want = HashSet::new();
            for id in visible.iter().filter(|id| ws.panes.contains_key(*id)) {
                let tab = ws.active_tab_pid(id);
                if !kasa_mcp::remote::is_view_pane(&tab) {
                    continue;
                }
                if self.mirror_needs_source(id, &tab) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_mirrors_take_source_size_until_a_reconstructed_view_is_chosen() {
        for kind in [MirrorKind::Chat, MirrorKind::Grid] {
            assert!(needs_source(kind, false, true));
        }
        assert!(!needs_source(MirrorKind::Chat, true, true));
        assert!(needs_source(MirrorKind::Grid, true, true), "셸 거울은 대화 보기를 켜도 원본 격자다");
        assert!(!needs_source(MirrorKind::Grid, false, false));
    }
}
