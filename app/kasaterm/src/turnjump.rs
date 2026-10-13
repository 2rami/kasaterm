//! 거울 칸의 질문 자리 이동 — Info 「내 프롬프트」가 거울에서 그 질문으로 옮길 때 쓴다(`info_prompts.rs`).
//! 거울은 받은 화면을 이 기기 스크롤백에 쌓으므로 절대 줄 하나로 한 번에 닿는다. 다시 접은 화면은 그
//! 줄을 맨 위에 세운다(`mirror_view::project_session_history_target`).

use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct TurnJump {
    /// 거울 탭 → 옮겨 간 절대 줄.
    mirror_targets: HashMap<String, i64>,
}

impl TurnJump {
    pub(crate) fn mirror_target(&self, pid: &str) -> Option<i64> { self.mirror_targets.get(pid).copied() }
    pub(crate) fn clear_mirror_target(&mut self, pid: &str) { self.mirror_targets.remove(pid); }

    /// 칸이 사라졌으면 목적지도 버린다 — 같은 번호가 재사용되면 남의 화면을 옮긴다.
    pub(crate) fn retain_panes(&mut self, alive: impl Fn(&str) -> bool) {
        self.mirror_targets.retain(|id, _| alive(id));
    }
}

impl crate::App {
    pub(crate) fn jump_mirror_abs(&mut self, pane: &str, abs: i64) -> bool {
        let pid = self.ws.lock().unwrap().active_tab_pid(pane);
        if !kasa_mcp::remote::is_view_pane(&pid) { return false; }
        if let Some(pty) = self.pty.get(&pid) {
            pty.scroll_to_abs(abs);
            self.mirror_view_scroll.remove(&pid);
            self.turn.mirror_targets.insert(pid, abs);
            self.chrome_dirty = true;
        }
        true
    }
}
