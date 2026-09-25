//! 방 배치 채널의 GUI 쪽 — 원본이면 이 기계의 트리를 싣고, 거울이면 받은 트리를 보기 창에
//! 그대로 앉힌다(`kasa_mcp::layout_feed`·`layout_watch`). 좌표에서 트리를 되짓는
//! `sync_remote_view_layouts` 는 채널이 없는 옛 판 원본용으로만 남는다.
use super::*;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// 원본 트리에서 이쪽이 비추는 칸만 남기고 번호를 이쪽 leaf 로 바꾼다. 못 비추는 칸(원본의
/// 거울, 아직 못 앉힌 새 칸)은 빠지고 그 형제가 자리를 채운다.
fn project(
    tree: &kasa_pty::PtyLayout,
    map: &mut impl FnMut(&str) -> Option<String>,
) -> Option<kasa_pty::PtyLayout> {
    match tree {
        kasa_pty::PtyLayout::Leaf { pane_id } => map(pane_id).map(kasa_pty::PtyLayout::single),
        kasa_pty::PtyLayout::Split { dir, ratio, a, b } => match (project(a, map), project(b, map)) {
            (Some(a), Some(b)) => Some(kasa_pty::PtyLayout::Split {
                dir: *dir,
                ratio: *ratio,
                a: Box::new(a),
                b: Box::new(b),
            }),
            (one, None) | (None, one) => one,
        },
    }
}

/// 모양·축·leaf 가 같고 비율이 사실상 같다.
fn same_layout(a: &kasa_pty::PtyLayout, b: &kasa_pty::PtyLayout) -> bool {
    use kasa_pty::PtyLayout::{Leaf, Split};
    match (a, b) {
        (Leaf { pane_id: x }, Leaf { pane_id: y }) => x == y,
        (Split { dir: d1, ratio: r1, a: a1, b: b1 }, Split { dir: d2, ratio: r2, a: a2, b: b2 }) => {
            d1 == d2 && (r1 - r2).abs() < 0.0005 && same_layout(a1, a2) && same_layout(b1, b2)
        }
        _ => false,
    }
}

/// leaf 에 든 PTY 번호들 — 탭을 빼낸 자리는 leaf 번호와 PTY 번호가 다르다.
fn leaf_ptys(ws: &Workspace, leaf: &str) -> Vec<String> {
    let mut ids: Vec<String> = ws.panes.get(leaf)
        .map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()).collect())
        .unwrap_or_default();
    if !ids.iter().any(|id| id == leaf) {
        ids.insert(0, leaf.to_string());
    }
    ids
}

/// 원본 leaf 에 든 PTY 번호들. 옛 원본이 `members` 를 비웠으면 leaf 번호 하나.
fn source_ptys<'a>(room: &'a kasa_mcp::layout_feed::Room, leaf: &'a str) -> Vec<&'a str> {
    match room.members.get(leaf) {
        Some(ids) if !ids.is_empty() => ids.iter().map(String::as_str).collect(),
        _ => vec![leaf],
    }
}

thread_local! {
    /// 창마다 마지막으로 맞춘 `(받은 배치, 이쪽 구성)` 지문. 같으면 다시 안 맞춘다 — 이쪽에서
    /// 옮긴 것이 원본에 닿기 전에 옛 배치로 되돌리지 않게.
    static APPLIED: std::cell::RefCell<HashMap<usize, u64>> = Default::default();
}

impl App {
    /// 원본 쪽: 방마다 트리를 채널에 싣는다. 바뀐 것만 나가니 매 루프 부른다 — 분할선 끌기처럼
    /// `publish_pty_layout` 을 안 거치는 변경도 여기서 잡힌다.
    pub(crate) fn publish_layout_feed(&self) {
        if self.tmux.is_some() {
            return;
        }
        let ws = self.ws.lock().unwrap();
        let rooms = self.windows.iter().enumerate().filter_map(|(window, slot)| {
            let tree = if window == self.active_window { self.pty_layout.as_ref() } else { slot.as_ref() }?;
            let members = tree.leaves().into_iter()
                .map(|leaf| (leaf.to_string(), leaf_ptys(&ws, leaf)))
                .collect();
            Some(kasa_mcp::layout_feed::Room { window, tree: tree.clone(), members })
        }).collect();
        drop(ws);
        kasa_mcp::layout_feed::publish(rooms);
    }

    /// 보기 창과 그 기계의 base.
    fn view_window_bases(&self) -> Vec<(usize, String)> {
        (0..self.windows.len()).filter_map(|i| {
            let (label, _) = self.remote_view_of_window(i)?;
            Some((i, kasa_mcp::machines::find(&label)?.base))
        }).collect()
    }

    /// 보기 창이 있는 기계에만 채널을 붙여 둔다.
    pub(crate) fn watch_view_machines(&self) {
        if self.tmux.is_some() {
            return;
        }
        let mut bases: Vec<String> = self.view_window_bases().into_iter().map(|(_, base)| base).collect();
        bases.sort();
        bases.dedup();
        kasa_mcp::layout_watch::retain(&bases);
    }

    /// 채널에 새 배치가 왔으면 맞춘다 — 매 루프.
    pub(crate) fn poll_layout_watch(&mut self) {
        static SEEN: AtomicU64 = AtomicU64::new(u64::MAX);
        let now = kasa_mcp::layout_watch::generation();
        if SEEN.swap(now, Ordering::Relaxed) != now {
            self.apply_remote_view_trees();
        }
    }

    /// 보기 창마다 원본 방 트리를 앉힌다. 채널이 산 기계의 창 번호를 돌려준다 — 좌표
    /// 폴링은 그 창을 건너뛴다.
    pub(crate) fn apply_remote_view_trees(&mut self) -> HashSet<usize> {
        let mut live = HashSet::new();
        let (mut changed_active, mut changed_any) = (false, false);
        for (i, base) in self.view_window_bases() {
            if !kasa_mcp::layout_watch::is_live(&base) {
                continue;
            }
            live.insert(i);
            let Some((stamp, rooms)) = kasa_mcp::layout_watch::settled(&base) else { continue };
            let Some((fresh, shape)) = self.project_source_room(i, &base, &rooms) else { continue };
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (stamp, shape).hash(&mut hasher);
            let print = hasher.finish();
            if APPLIED.with(|a| a.borrow_mut().insert(i, print)) == Some(print) {
                continue;
            }
            let tree = if i == self.active_window { self.pty_layout.as_mut() }
                else { self.windows.get_mut(i).and_then(Option::as_mut) };
            let Some(tree) = tree else { continue };
            if same_layout(tree, &fresh) {
                continue;
            }
            *tree = fresh;
            changed_any = true;
            changed_active |= i == self.active_window;
        }
        if changed_active {
            let leaves = self.window_leaves(self.active_window);
            if self.zoomed_pane.as_ref().is_some_and(|z| !leaves.contains(z)) {
                self.zoomed_pane = None;
            }
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
        }
        if changed_any {
            self.publish_pty_layout();
            self.session_touched = true;
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        live
    }

    /// `i` 번 보기 창에 앉힐 트리와, 그 판정에 쓴 이쪽 구성. 이쪽 leaf 가 원본 방 하나에
    /// 한 번씩 다 들어맞을 때만 — 연결 중인 새 칸(아직 원격 번호가 없다)이 있으면 기다린다.
    fn project_source_room(
        &self,
        i: usize,
        base: &str,
        rooms: &[kasa_mcp::layout_feed::Room],
    ) -> Option<(kasa_pty::PtyLayout, Vec<(String, Vec<String>)>)> {
        let leaves = self.window_leaves(i);
        // 원격 조회는 ws 를 놓고 한다 — 링크 잠금과 순서가 엇갈리지 않게.
        let per_leaf: Vec<(String, Vec<String>)> = {
            let ws = self.ws.lock().unwrap();
            leaves.iter().map(|leaf| (leaf.clone(), leaf_ptys(&ws, leaf))).collect()
        };
        let mut owner: HashMap<String, String> = HashMap::new();
        let mut shape = Vec::with_capacity(per_leaf.len());
        for (leaf, ids) in per_leaf {
            let remote: Vec<String> = ids.iter()
                .filter_map(|id| kasa_mcp::remote::remote_info(id))
                .filter(|info| kasa_mcp::machines::same_machine_bases(&info.base, base))
                .map(|info| info.remote_id)
                .collect();
            for id in &remote {
                owner.insert(id.clone(), leaf.clone());
            }
            shape.push((leaf, remote));
        }
        let touches = |room: &kasa_mcp::layout_feed::Room| room.tree.leaves().iter()
            .any(|leaf| source_ptys(room, leaf).iter().any(|id| owner.contains_key(*id)));
        let mut hits = rooms.iter().filter(|room| touches(room));
        let room = hits.next()?;
        if hits.next().is_some() {
            return None;
        }
        let mut used: HashSet<String> = HashSet::new();
        let mut twice = false;
        let fresh = project(&room.tree, &mut |leaf| {
            let local = source_ptys(room, leaf).iter().find_map(|id| owner.get(*id)).cloned()?;
            twice |= !used.insert(local.clone());
            Some(local)
        })?;
        // 지문은 순서와 무관하게 — 이쪽에서 자리만 바꾼 것은 원본이 답할 때까지 둔다.
        shape.sort();
        (!twice && used.len() == leaves.len()).then_some((fresh, shape))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_pty::{PtyLayout, SplitDir};

    fn split(dir: SplitDir, ratio: f32, a: PtyLayout, b: PtyLayout) -> PtyLayout {
        PtyLayout::Split { dir, ratio, a: Box::new(a), b: Box::new(b) }
    }

    #[test]
    fn project_keeps_source_axis_and_drops_unmirrored_cells() {
        // 원본: 위가 %1|%2, 아래가 %3 — 가로선부터 자른 모양. 좌표로는 세로선부터 되짓던 것.
        let source = split(
            SplitDir::Vertical, 0.6,
            split(SplitDir::Horizontal, 0.3, PtyLayout::single("%1"), PtyLayout::single("%2")),
            PtyLayout::single("%3"),
        );
        let map = HashMap::from([("%1", "%10"), ("%2", "%20"), ("%3", "%30")]);
        let got = project(&source, &mut |id| map.get(id).map(|s| s.to_string())).unwrap();
        let want = split(
            SplitDir::Vertical, 0.6,
            split(SplitDir::Horizontal, 0.3, PtyLayout::single("%10"), PtyLayout::single("%20")),
            PtyLayout::single("%30"),
        );
        assert!(same_layout(&got, &want));
        // %2 를 안 비추면 %1 이 그 줄을 채운다.
        let partial = project(&source, &mut |id| (id != "%2").then(|| map[id].to_string())).unwrap();
        let want = split(SplitDir::Vertical, 0.6, PtyLayout::single("%10"), PtyLayout::single("%30"));
        assert!(same_layout(&partial, &want));
        assert!(project(&source, &mut |_| None).is_none());
    }

    #[test]
    fn same_layout_ignores_float_noise_but_not_axis() {
        let a = split(SplitDir::Horizontal, 0.5, PtyLayout::single("%1"), PtyLayout::single("%2"));
        let b = split(SplitDir::Horizontal, 0.5001, PtyLayout::single("%1"), PtyLayout::single("%2"));
        let c = split(SplitDir::Vertical, 0.5, PtyLayout::single("%1"), PtyLayout::single("%2"));
        assert!(same_layout(&a, &b));
        assert!(!same_layout(&a, &c));
    }
}
