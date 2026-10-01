//! 이 기기 방 지키기 — 다른 기기 방의 보기 창에 자기 칸을 섞지 않고, 자기 칸이 하나도
//! 없으면 이 기기 방을 하나 세운다.
//!
//! 사이드바는 방 단위로 기기를 가른다. 방의 칸이 전부 한 기기의 view 거울이어야 그 기기
//! 절의 방으로 서고(`remote_view_of_window`), 하나라도 자기 칸이면 방 통째로 「이 기기」
//! 절에 선다. 그래서 보기 창에 자기 칸이 하나 끼면 저쪽 학생들까지 이 기기 학생처럼
//! 보였다(2026-10-01 맥미니: 재부팅 뒤 자기 칸 없이 맥북 보기 창만 남은 데로 이사 온
//! 학생·`/spawn-shell` 셸이 활성 칸 옆에 앉았다). 섞이는 길을 막고, 자기 칸이 0 이 되는
//! 모든 길(마지막 자기 칸 닫기·이사·복원)은 틱 하나가 받는다.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RoomKind {
    /// 보기 창도 내부 방도 아닌 방 — 사이드바 「이 기기」 절에 서는 방.
    Own,
    View,
    Internal,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RoomFacts {
    pub(crate) kind: RoomKind,
    /// 다른 기기에 몸이 없는 칸 수 — 탭·별도창 칸 포함.
    pub(crate) own_panes: usize,
}

/// 방이 있는데 자기 칸이 하나도 없다. 방이 아예 없으면 기동 중이라 셈하지 않는다.
pub(crate) fn lacks_own_pane(rooms: &[RoomFacts]) -> bool {
    !rooms.is_empty() && rooms.iter().all(|r| r.own_panes == 0)
}

/// 자기 칸을 앉힐 이 기기 방 — 지금 보는 방이 이 기기 방이면 그 방, 아니면 앞 번호.
pub(crate) fn own_host_room(rooms: &[RoomFacts], active: usize) -> Option<usize> {
    if rooms.get(active).is_some_and(|r| r.kind == RoomKind::Own) {
        return Some(active);
    }
    rooms.iter().position(|r| r.kind == RoomKind::Own)
}

const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

impl App {
    pub(crate) fn room_facts(&self) -> Vec<RoomFacts> {
        (0..self.windows.len())
            .map(|i| {
                if self.internal_room_kind_at(i).is_some() {
                    return RoomFacts { kind: RoomKind::Internal, own_panes: 0 };
                }
                let kind = if self.remote_view_of_window(i).is_some() { RoomKind::View } else { RoomKind::Own };
                let mut ids = self.window_leaves(i);
                {
                    let ws = self.ws.lock().unwrap();
                    let tabs: Vec<String> = ids
                        .iter()
                        .filter_map(|leaf| ws.panes.get(leaf))
                        .flat_map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()))
                        .collect();
                    ids.extend(tabs);
                }
                ids.extend(self.room_undocked(i));
                ids.sort();
                ids.dedup();
                let own_panes = ids.iter().filter(|id| kasa_mcp::remote::remote_info(id).is_none()).count();
                RoomFacts { kind, own_panes }
            })
            .collect()
    }

    /// 이 기기에 세울 칸(현지 셸·학생·연결 칸)이 앉을 기준. 기준이 보기 창이면 이 기기
    /// 방의 칸으로 바꾸고, 이 기기 방이 없으면 하나 연다. 보기 창 안의 사람 분할은 원본
    /// 기기에 세우는 거울이라(`spawn_inherited_remote_session`) 여기를 안 지난다.
    pub(crate) fn own_spawn_host(&mut self, host: &str) -> String {
        let outer = self.ws.lock().unwrap().outer_for_pty(host).unwrap_or_else(|| host.to_string());
        let Some(window) = self.window_of_pane(&outer) else { return outer };
        if self.remote_view_of_window(window).is_none() {
            return outer;
        }
        let Some(room) = own_host_room(&self.room_facts(), self.active_window) else {
            return self.open_own_room().unwrap_or(outer);
        };
        let focused = {
            let ws = self.ws.lock().unwrap();
            ws.active_pane.clone().map(|p| ws.outer_for_pty(&p).unwrap_or(p))
        }
        .filter(|p| room == self.active_window && self.window_of_pane(p) == Some(room));
        focused.or_else(|| self.window_leaves(room).into_iter().next()).unwrap_or(outer)
    }

    /// 지금 세울 칸이 이 기기 칸인가 — 기계·학생을 지정한 스폰은 거울 옆이어도 현지에 선다.
    pub(crate) fn spawns_own_pane(&self) -> bool {
        self.pending_spawn_cwd.is_some() || self.pending_character.is_some()
    }

    /// 자기 칸이 0 이면 이 기기 방을 하나 세운다. 보던 방은 그대로 둔다.
    pub(crate) fn keep_own_room(&mut self) {
        use std::sync::{Mutex, OnceLock};
        static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
        // 복원 대화상자가 떠 있으면 아직 부팅 셸만 있고, 설정 방에선 방을 바꾸면 설정이 닫힌다.
        // 문서 뷰어는 칸이 없는 앱이고, 칸이 하나도 없으면 아직 첫 셸을 띄우기 전이다.
        if self.tmux.is_some() || self.lite || self.viewer_only || self.pty.is_empty()
            || self.restore_prompt.is_some()
            || self.close_freeze.live() || self.internal_room_active_any()
        {
            return;
        }
        {
            let mut last = LAST.get_or_init(|| Mutex::new(None)).lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < CHECK_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        if !lacks_own_pane(&self.room_facts()) {
            return;
        }
        eprintln!("[own-room] 자기 칸이 없어 이 기기 방을 세운다");
        self.open_own_room();
    }

    /// 새 이 기기 방(셸 하나)을 열고 보던 방으로 돌아간다. 새 방 첫 칸을 돌려준다.
    fn open_own_room(&mut self) -> Option<String> {
        let back = self.active_window;
        self.new_window();
        let created = self.active_window;
        let pane = self.window_leaves(created).into_iter().next();
        if back != created {
            self.switch_window(back);
        }
        pane
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(kind: RoomKind, own_panes: usize) -> RoomFacts {
        RoomFacts { kind, own_panes }
    }

    #[test]
    fn only_mirrors_left_needs_an_own_room() {
        assert!(lacks_own_pane(&[room(RoomKind::View, 0), room(RoomKind::View, 0)]));
        assert!(lacks_own_pane(&[room(RoomKind::View, 0), room(RoomKind::Internal, 0)]),
            "설정 방은 자기 칸이 아니다");
        assert!(lacks_own_pane(&[room(RoomKind::Own, 0)]),
            "이사·연결로 몸이 다 저쪽에 간 방도 자기 칸이 없다");
        assert!(!lacks_own_pane(&[room(RoomKind::View, 0), room(RoomKind::Own, 1)]));
        assert!(!lacks_own_pane(&[]), "방이 없으면 기동 중이다");
    }

    #[test]
    fn own_panes_never_land_in_a_view_room() {
        let rooms = [room(RoomKind::View, 0), room(RoomKind::Internal, 0), room(RoomKind::Own, 1), room(RoomKind::Own, 2)];
        assert_eq!(own_host_room(&rooms, 0), Some(2), "보기 창을 보고 있으면 앞 번호 이 기기 방");
        assert_eq!(own_host_room(&rooms, 3), Some(3), "이 기기 방을 보고 있으면 그 방");
        assert_eq!(own_host_room(&rooms, 1), Some(2), "설정 방은 자리가 아니다");
        assert_eq!(own_host_room(&[room(RoomKind::View, 0)], 0), None, "없으면 새로 연다");
    }

    /// 자기 칸을 트리에 꽂는 길은 전부 꽂기 전에 기준을 이 기기 방으로 바꾼다.
    #[test]
    fn every_own_spawn_reanchors_before_planting() {
        fn between<'a>(src: &'a str, start: &str, end: &str) -> &'a str {
            src.split_once(start).unwrap_or_else(|| panic!("missing {start}")).1
                .split_once(end).unwrap_or_else(|| panic!("missing {end}")).0
        }
        fn before(body: &str, name: &str, plant: &str) {
            let guard = body.find("own_spawn_host(").unwrap_or_else(|| panic!("{name}: 기준 바꾸기 없음"));
            let planted = body.find(plant).unwrap_or_else(|| panic!("{name}: {plant} 없음"));
            assert!(guard < planted, "{name}: 기준을 바꾼 뒤에 꽂아야 한다");
        }
        let layout = include_str!("layout.rs");
        let session = include_str!("session.rs");
        before(between(layout, "fn split_active_pane_as(", "pub(crate) fn split_active_pane_focused"),
            "split_active_pane_as", "spawn_split_session(");
        before(between(layout, "pub(crate) fn split_fleet(", "fn live_remote_anchor"), "split_fleet", "spawn_split_session(");
        before(between(layout, "fn split_beside(", "pub(crate) fn spawn_split_session"), "split_beside", "spawn_split_session(");
        before(between(layout, "pub(crate) fn spawn_new_tab(", "pub(crate) fn pane_cells"), "spawn_new_tab", "spawn_inherited_remote_session(");
        before(between(session, "pub(crate) fn spawn_remote_pane", "pub(crate) fn mirror_remote_pane"),
            "spawn_remote_pane", "remote::connect(");
    }
}
