//! Pure discovery policy. A snapshot is authoritative only while directly online.
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourcePane {
    pub id: String,
    pub room: u64,
    pub eligible: bool,
}

#[derive(Default)]
struct SourceState {
    following: bool,
    seen: HashSet<String>,
    rooms: HashSet<u64>,
}

#[derive(Default)]
pub(crate) struct Planner {
    sources: HashMap<String, SourceState>,
}

impl Planner {
    pub fn observe(
        &mut self,
        source: &str,
        following: bool,
        fresh: bool,
        panes: &[SourcePane],
        occupied_rooms: &HashSet<u64>,
    ) -> Vec<SourcePane> {
        let state = self.sources.entry(source.into()).or_default();
        if !following {
            state.following = false;
            return Vec::new();
        }
        if !fresh { return Vec::new(); }
        let baseline = !state.following;
        state.following = true;
        let old_rooms = state.rooms.clone();
        let mut added = Vec::new();
        for pane in panes {
            let new = state.seen.insert(pane.id.clone());
            state.rooms.insert(pane.room);
            // Seen includes closed panes and mirrors: reopening a historical source
            // or removing its mirror marker must not create an unsolicited viewer.
            if !baseline && new && pane.eligible
                && (!old_rooms.contains(&pane.room) || occupied_rooms.contains(&pane.room))
            {
                added.push(pane.clone());
            }
        }
        // Local-only closes leave the source in this complete snapshot, hence
        // remain seen. Actual source deletion permits later reuse of its number.
        // Never do this on an offline/empty cache (returned above).
        state.seen.retain(|id| panes.iter().any(|pane| &pane.id == id));
        state.rooms.retain(|room| panes.iter().any(|pane| &pane.room == room));
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pane(id: &str, room: u64) -> SourcePane {
        SourcePane { id: id.into(), room, eligible: true }
    }
    fn observe(p: &mut Planner, panes: &[SourcePane], rooms: &[u64]) -> Vec<SourcePane> {
        p.observe("host", true, true, panes, &rooms.iter().copied().collect())
    }
    #[test]
    fn baseline_then_new_pane_and_new_room_only_once() {
        let mut p = Planner::default();
        assert!(observe(&mut p, &[pane("%0", 0), pane("%old", 8)], &[0]).is_empty());
        let rows = [pane("%0", 0), pane("%1", 0), pane("%2", 1), pane("%old", 8)];
        assert_eq!(observe(&mut p, &rows, &[0]), vec![pane("%1", 0), pane("%2", 1)]);
        assert!(observe(&mut p, &rows, &[0, 1]).is_empty());
    }
    #[test]
    fn closing_local_view_or_whole_room_does_not_resurrect_it() {
        let mut p = Planner::default();
        observe(&mut p, &[pane("%0", 0), pane("%1", 1)], &[0, 1]);
        assert!(observe(&mut p, &[pane("%0", 0), pane("%1", 1), pane("%2", 1)], &[0]).is_empty());
    }
    #[test]
    fn offline_empty_snapshot_is_not_a_new_baseline() {
        let mut p = Planner::default();
        observe(&mut p, &[pane("%0", 0)], &[0]);
        p.observe("host", true, false, &[], &HashSet::new());
        assert_eq!(observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]), vec![pane("%1", 0)]);
    }
    #[test]
    fn never_follows_mirrors_closed_rows_or_historical_reopening() {
        let mut p = Planner::default();
        observe(&mut p, &[pane("%0", 0)], &[0]);
        let mut hidden = pane("%1", 0); hidden.eligible = false;
        assert!(observe(&mut p, &[pane("%0", 0), hidden], &[0]).is_empty());
        assert!(observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]).is_empty());
    }
    #[test]
    fn restarting_follow_uses_a_new_baseline() {
        let mut p = Planner::default();
        observe(&mut p, &[pane("%0", 0)], &[0]);
        p.observe("host", false, true, &[], &HashSet::new());
        assert!(observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]).is_empty());
    }
    #[test]
    fn source_deleted_number_can_be_reused_but_local_hidden_number_stays_seen() {
        let mut p = Planner::default();
        observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]);
        assert!(observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]).is_empty());
        observe(&mut p, &[pane("%0", 0)], &[0]);
        assert_eq!(observe(&mut p, &[pane("%0", 0), pane("%1", 0)], &[0]), vec![pane("%1", 0)]);
    }
}
