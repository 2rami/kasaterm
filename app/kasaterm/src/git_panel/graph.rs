use kasa_mcp::git::GitGraphCommit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Track {
    pub lane: usize,
    pub color: usize,
    pub target: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub lane: usize,
    pub color: usize,
    pub incoming: bool,
    pub passing: Vec<Track>,
    pub parents: Vec<Track>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Layout {
    pub rows: Vec<Row>,
    pub columns: usize,
    pub boundary: Vec<Track>,
}

fn vacant(lanes: &mut Vec<Option<(String, usize)>>) -> usize {
    if let Some(index) = lanes.iter().position(Option::is_none) {
        return index;
    }
    lanes.push(None);
    lanes.len() - 1
}

pub(super) fn layout(commits: &[GitGraphCommit]) -> Layout {
    let mut result = Layout::default();
    let mut lanes: Vec<Option<(String, usize)>> = Vec::new();
    let mut next_color = 0;
    for commit in commits {
        let existing = lanes
            .iter()
            .position(|lane| lane.as_ref().is_some_and(|(oid, _)| oid == &commit.oid));
        let lane = existing.unwrap_or_else(|| vacant(&mut lanes));
        let color = if let Some((_, color)) = &lanes[lane] {
            *color
        } else {
            let color = next_color;
            next_color += 1;
            color
        };
        let passing = lanes
            .iter()
            .enumerate()
            .filter_map(|(index, value)| {
                let (target, color) = value.as_ref()?;
                (index != lane).then(|| Track {
                    lane: index,
                    color: *color,
                    target: target.clone(),
                })
            })
            .collect();
        lanes[lane] = None;
        let mut parents = Vec::new();
        for oid in &commit.parents {
            if oid == &commit.oid || parents.iter().any(|track: &Track| &track.target == oid) {
                continue;
            }
            let parent_lane = lanes
                .iter()
                .position(|entry| entry.as_ref().is_some_and(|(target, _)| target == oid));
            let parent_lane = parent_lane.unwrap_or_else(|| {
                let slot = if parents.is_empty() && lanes[lane].is_none() {
                    lane
                } else {
                    vacant(&mut lanes)
                };
                let parent_color = if parents.is_empty() {
                    color
                } else {
                    let color = next_color;
                    next_color += 1;
                    color
                };
                lanes[slot] = Some((oid.clone(), parent_color));
                slot
            });
            let parent_color = lanes[parent_lane].as_ref().unwrap().1;
            parents.push(Track {
                lane: parent_lane,
                color: parent_color,
                target: oid.clone(),
            });
        }
        result.columns = result.columns.max(lanes.len()).max(lane + 1);
        result.rows.push(Row {
            lane,
            color,
            incoming: existing.is_some(),
            passing,
            parents,
        });
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
    }
    result.boundary = lanes
        .into_iter()
        .enumerate()
        .filter_map(|(lane, entry)| {
            entry.map(|(target, color)| Track {
                lane,
                color,
                target,
            })
        })
        .collect();
    result
}

pub(super) fn graph_width(columns: usize, width: f32) -> f32 {
    ((columns.max(1) as f32 - 1.0) * 14.0 + 20.0)
        .min(width.max(0.0) * 0.4)
        .min(112.0)
}

pub(super) fn lane_x(lane: usize, columns: usize, width: f32) -> f32 {
    let span = (width - 12.0).max(0.0);
    6.0_f32.min(width / 2.0)
        + if columns <= 1 {
            0.0
        } else {
            (span / (columns - 1) as f32).min(14.0) * lane as f32
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn commit(oid: &str, parents: &[&str]) -> GitGraphCommit {
        GitGraphCommit {
            oid: oid.into(),
            parents: parents.iter().map(|id| (*id).into()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn first_parent_stays_in_one_lane_and_root_stops() {
        let result = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
        assert_eq!(result.columns, 1);
        assert!(result.boundary.is_empty());
        assert!(!result.rows[0].incoming);
        assert!(result.rows[1].incoming && result.rows[2].incoming);
        assert!(result
            .rows
            .iter()
            .all(|row| row.lane == 0 && row.color == 0));
    }

    #[test]
    fn fork_and_merge_connect_actual_parent_oids_without_duplicate_rails() {
        let result = layout(&[
            commit("merge", &["main", "topic"]),
            commit("topic", &["base"]),
            commit("main", &["base"]),
            commit("base", &[]),
        ]);
        assert_eq!(result.columns, 2);
        assert_eq!(
            result.rows[0]
                .parents
                .iter()
                .map(|p| p.target.as_str())
                .collect::<Vec<_>>(),
            ["main", "topic"]
        );
        assert_eq!(result.rows[1].lane, 1);
        assert_eq!(result.rows[2].parents[0].lane, result.rows[3].lane);
        assert!(result.boundary.is_empty());
    }

    #[test]
    fn octopus_retains_all_edges_and_reuses_shared_ancestor() {
        let result = layout(&[
            commit("octopus", &["a", "b", "c"]),
            commit("a", &["root"]),
            commit("b", &["root"]),
            commit("c", &["root"]),
            commit("root", &[]),
        ]);
        assert_eq!(result.rows[0].parents.len(), 3);
        assert_eq!(result.columns, 3);
        assert!(result.boundary.is_empty());
        for row in &result.rows[1..4] {
            assert_eq!(row.parents[0].lane, result.rows[4].lane);
        }
    }

    #[test]
    fn truncated_history_keeps_unknown_parent_as_boundary_not_fake_root() {
        let result = layout(&[commit("tip", &["outside", "other"])]);
        assert_eq!(
            result
                .boundary
                .iter()
                .map(|p| p.target.as_str())
                .collect::<Vec<_>>(),
            ["outside", "other"]
        );
        assert_eq!(result.rows[0].parents.len(), 2);
    }

    #[test]
    fn disconnected_tips_and_duplicate_parent_inputs_remain_distinct() {
        let result = layout(&[
            commit("one", &["root", "root"]),
            commit("two", &["other"]),
            commit("root", &[]),
            commit("other", &[]),
        ]);
        assert_eq!(result.rows[0].parents.len(), 1);
        assert_ne!(result.rows[0].color, result.rows[1].color);
        assert_eq!(result.rows[1].passing[0].target, "root");
        assert!(result.boundary.is_empty());
    }

    #[test]
    fn graph_geometry_leaves_text_room_at_narrow_widths() {
        for width in [0.0, 80.0, 292.0, 320.0, 600.0] {
            for columns in [1, 2, 8, 200] {
                let graph = graph_width(columns, width);
                assert!(graph <= width * 0.4);
                for lane in 0..columns {
                    let x = lane_x(lane, columns, graph);
                    assert!(x >= 0.0 && x <= graph);
                }
            }
        }
    }
}
