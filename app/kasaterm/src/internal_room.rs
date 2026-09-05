//! PTY 없는 네이티브 내부 방의 공통 수명 규칙.
//!
//! 설정과 보드는 사용자 작업 방과 같은 `windows` 목록에 앉지만 셸, cwd, 복원
//! 기록을 갖지 않는다. 이 모듈은 내부 방의 marker와 변형 차단을 한 관문으로
//! 묶어, 새 기능이 내부 화면을 터미널 pane으로 오인하지 않게 한다.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum InternalRoomKind {
    Settings,
    Board,
}

impl InternalRoomKind {
    pub(crate) const ALL: [Self; 2] = [Self::Settings, Self::Board];

    pub(crate) const fn pane_id(self) -> &'static str {
        match self {
            Self::Settings => "\0kasaterm-settings",
            Self::Board => "\0kasaterm-board",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Settings => "설정",
            Self::Board => "보드",
        }
    }

    pub(crate) const fn saved_name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Board => "board",
        }
    }

    pub(crate) fn from_pane(pane: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| pane == kind.pane_id())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InternalMutation {
    Split,
    Tab,
    Merge,
    Move,
    FilePreview,
    WebPane,
    RemotePane,
    RemoteMirror,
    Migrate,
}

impl InternalMutation {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::Split,
        Self::Tab,
        Self::Merge,
        Self::Move,
        Self::FilePreview,
        Self::WebPane,
        Self::RemotePane,
        Self::RemoteMirror,
        Self::Migrate,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Tab => "tab",
            Self::Merge => "merge",
            Self::Move => "move",
            Self::FilePreview => "file preview",
            Self::WebPane => "web pane",
            Self::RemotePane => "remote pane",
            Self::RemoteMirror => "remote mirror",
            Self::Migrate => "migrate",
        }
    }
}

pub(crate) fn kind_of_layout(layout: &kasa_pty::PtyLayout) -> Option<InternalRoomKind> {
    let leaves = layout.leaves();
    (leaves.len() == 1)
        .then(|| InternalRoomKind::from_pane(leaves[0]))
        .flatten()
}

pub(crate) fn is_layout(layout: &kasa_pty::PtyLayout, kind: InternalRoomKind) -> bool {
    kind_of_layout(layout) == Some(kind)
}

pub(crate) fn should_persist_layout(layout: &kasa_pty::PtyLayout) -> bool {
    kind_of_layout(layout).is_none()
}

pub(crate) fn pane_operation_allowed(pane: &str, _mutation: InternalMutation) -> bool {
    InternalRoomKind::from_pane(pane).is_none()
}

pub(crate) fn is_saved_window(node: &serde_json::Value) -> bool {
    node.get("leaf").is_some_and(|leaf| {
        let saved = leaf.get("internal").and_then(|value| value.as_str());
        let pane = leaf.get("pane_id").and_then(|value| value.as_str());
        InternalRoomKind::ALL
            .into_iter()
            .any(|kind| saved == Some(kind.saved_name()) || pane == Some(kind.pane_id()))
    })
}

pub(crate) fn remap_after_removal(index: usize, removed: usize) -> Option<usize> {
    if index == removed {
        None
    } else {
        Some(index.saturating_sub(usize::from(index > removed)))
    }
}

impl App {
    pub(crate) fn internal_room_index(&self, kind: InternalRoomKind) -> Option<usize> {
        (0..self.windows.len()).find(|&idx| {
            let layout = if idx == self.active_window {
                self.pty_layout.as_ref()
            } else {
                self.windows.get(idx).and_then(Option::as_ref)
            };
            layout.is_some_and(|layout| is_layout(layout, kind))
        })
    }

    pub(crate) fn internal_room_active(&self, kind: InternalRoomKind) -> bool {
        self.internal_room_index(kind) == Some(self.active_window)
    }

    pub(crate) fn internal_room_active_any(&self) -> bool {
        self.internal_room_kind_at(self.active_window).is_some()
    }

    pub(crate) fn pane_is_internal(&self, pane: &str) -> bool {
        InternalRoomKind::from_pane(pane).is_some()
    }

    pub(crate) fn internal_room_kind_at(&self, idx: usize) -> Option<InternalRoomKind> {
        let layout = if idx == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(idx).and_then(Option::as_ref)
        };
        layout.and_then(kind_of_layout)
    }

    pub(crate) fn internal_room_count(&self) -> usize {
        (0..self.windows.len())
            .filter(|idx| self.internal_room_kind_at(*idx).is_some())
            .count()
    }

    pub(crate) fn active_user_pane(&self) -> Option<String> {
        self.ws
            .lock()
            .ok()
            .and_then(|ws| ws.active_pane.clone())
            .filter(|pane| InternalRoomKind::from_pane(pane).is_none())
    }

    pub(crate) fn internal_return_pane(&self, kind: InternalRoomKind) -> Option<&str> {
        match kind {
            InternalRoomKind::Settings => self.settings_scene.return_pane(),
            InternalRoomKind::Board => self.board_scene.return_pane(),
        }
    }

    pub(crate) fn return_from_active_internal_room(&mut self) -> bool {
        match self.internal_room_kind_at(self.active_window) {
            Some(InternalRoomKind::Settings) => self.return_from_settings_room(),
            Some(InternalRoomKind::Board) => self.return_from_board_room(),
            None => false,
        }
    }

    pub(crate) fn ensure_user_mutation_target(
        &self,
        pane: &str,
        mutation: InternalMutation,
    ) -> anyhow::Result<()> {
        if !pane_operation_allowed(pane, mutation) {
            let room = InternalRoomKind::from_pane(pane)
                .map(InternalRoomKind::label)
                .unwrap_or("내부");
            anyhow::bail!("{room} 방은 {} 대상이 될 수 없다", mutation.label());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_internal_rooms_share_the_nonpersist_contract() {
        for kind in InternalRoomKind::ALL {
            let layout = kasa_pty::PtyLayout::single(kind.pane_id());
            assert_eq!(kind_of_layout(&layout), Some(kind));
            assert!(!should_persist_layout(&layout));
            for mutation in InternalMutation::ALL {
                assert!(!pane_operation_allowed(kind.pane_id(), mutation));
            }
        }
        let user = kasa_pty::PtyLayout::single("%1");
        assert!(should_persist_layout(&user));
    }

    #[test]
    fn persisted_internal_markers_are_ignored_by_name_or_pane_id() {
        for kind in InternalRoomKind::ALL {
            assert!(is_saved_window(&serde_json::json!({
                "leaf": { "internal": kind.saved_name() }
            })));
            assert!(is_saved_window(&serde_json::json!({
                "leaf": { "pane_id": kind.pane_id() }
            })));
        }
        assert!(!is_saved_window(&serde_json::json!({
            "leaf": { "pane_id": "%1" }
        })));
    }
}
