use super::*;
use kasa_mcp::git::GitGraphCommit;

fn row_height(commit: &GitGraphCommit) -> f32 {
    if commit.refs.is_empty() {
        40.0
    } else {
        58.0
    }
}

fn commits(view: &GitColView) -> Vec<GitGraphCommit> {
    if view.graph_supported {
        return view.commit_graph.clone();
    }
    view.recent_commits
        .iter()
        .map(|(oid, subject)| GitGraphCommit {
            oid: oid.clone(),
            subject: subject.clone(),
            ..Default::default()
        })
        .collect()
}

fn expanded_height(git: &state::GitState, oid: &str) -> f32 {
    if git.col_commit_expanded.as_deref() != Some(oid) {
        return 0.0;
    }
    let Some(files) = git.col_commit_files_cache.get(oid) else {
        return 26.0;
    };
    if files.is_empty() {
        return 26.0;
    }
    files
        .iter()
        .map(|(path, _, _)| {
            let key = (oid.to_owned(), path.clone());
            let diff = if git.col_commit_file_expanded.contains(&key) {
                git.col_commit_diff_cache
                    .get(&key)
                    .map_or(18.0, |lines| lines.len().max(1) as f32 * 18.0)
            } else {
                0.0
            };
            26.0 + diff
        })
        .sum()
}

pub(crate) fn requested_history_count(_height: f32) -> usize {
    200
}

pub(crate) fn history_height(view: &GitColView, git: &state::GitState, cap: f32) -> f32 {
    if view.no_repo || view.loading || view.issue.is_some() {
        return 0.0;
    }
    let commits = commits(view);
    let auto = native_controls::CONTROL_HEIGHT
        + 8.0
        + commits
            .iter()
            .take(5)
            .map(row_height)
            .sum::<f32>()
            .max(40.0)
        + if view.graph_truncated { 26.0 } else { 0.0 };
    git.col_commits_h.unwrap_or(auto).max(0.0).min(cap.max(0.0))
}

fn lane_color(index: usize) -> [u8; 4] {
    let colors = [
        theme::accent(),
        theme::syn_string(),
        theme::syn_keyword(),
        theme::syn_number(),
        theme::syn_type(),
        theme::syn_function(),
    ];
    theme::enforce_contrast_at(colors[index % colors.len()], theme::panel_bg(), 3.0)
}

fn segment(g: &mut gpu::GpuRenderer, from: (f32, f32), to: (f32, f32), color: [u8; 4]) {
    if (from.0 - to.0).abs() < 0.1 {
        g.rect(
            from.0 - 1.0,
            from.1.min(to.1),
            2.0,
            (to.1 - from.1).abs(),
            color,
        );
        return;
    }
    let steps = (to.1 - from.1).abs().ceil().max(1.0) as usize;
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        let smooth = t * t * (3.0 - 2.0 * t);
        let x = from.0 + (to.0 - from.0) * smooth;
        let y = from.1 + (to.1 - from.1) * t;
        g.rect(x - 1.0, y, 2.0, 1.5, color);
    }
}

fn rails(
    g: &mut gpu::GpuRenderer,
    row: &graph::Row,
    columns: usize,
    x: f32,
    y: f32,
    graph_width: f32,
    height: f32,
) {
    let center_y = y + 13.0;
    let lane = |index| x + graph::lane_x(index, columns, graph_width);
    for track in &row.passing {
        segment(
            g,
            (lane(track.lane), y),
            (lane(track.lane), y + height),
            lane_color(track.color),
        );
    }
    if row.incoming {
        segment(
            g,
            (lane(row.lane), y),
            (lane(row.lane), center_y),
            lane_color(row.color),
        );
    }
    for parent in &row.parents {
        let join_y = (center_y + 18.0).min(y + height);
        segment(
            g,
            (lane(row.lane), center_y),
            (lane(parent.lane), join_y),
            lane_color(parent.color),
        );
        segment(
            g,
            (lane(parent.lane), join_y),
            (lane(parent.lane), y + height),
            lane_color(parent.color),
        );
    }
    circle_rect(
        g,
        lane(row.lane) - 5.0,
        center_y - 5.0,
        10.0,
        theme::panel_bg(),
    );
    circle_rect(
        g,
        lane(row.lane) - 3.0,
        center_y - 3.0,
        6.0,
        lane_color(row.color),
    );
}

fn ref_label(reference: &str) -> (&str, &str) {
    if reference == "HEAD" {
        ("check", "HEAD")
    } else if let Some(branch) = reference.strip_prefix("refs/heads/") {
        ("git-branch", branch)
    } else if let Some(branch) = reference.strip_prefix("refs/remotes/") {
        ("server", branch)
    } else {
        ("git-branch", reference)
    }
}

fn refs(g: &mut gpu::GpuRenderer, references: &[String], x: f32, y: f32, width: f32) {
    let mut bx = x;
    for (index, reference) in references.iter().enumerate() {
        let (icon, name) = ref_label(reference);
        let remaining = (x + width - bx).max(0.0);
        if remaining < 40.0 {
            break;
        }
        let tail = references.len() - index - 1;
        let available = if tail > 0 {
            (remaining - 32.0).max(40.0)
        } else {
            remaining
        };
        let text = info::fit_text(g, name, (available - 24.0).max(0.0), 9.5, false);
        let w = (g.measure_chrome_text(&text, 9.5, false) + 24.0).min(available);
        let color = if reference == "HEAD" || reference.starts_with("refs/heads/") {
            theme::enforce_contrast_at(theme::accent(), theme::panel_bg(), 4.5)
        } else {
            theme::text_dim()
        };
        g.round_rect_stroke(bx, y, w, 18.0, theme::radius_sm(), 1.0, theme::border());
        g.queue_icon(icon, bx + 4.0, y + 3.0, 12.0, color);
        label(g, bx + 19.0, y + 3.0, &text, w - 22.0, 9.5, color, false);
        bx += w + 4.0;
        if tail > 0 && x + width - bx < 40.0 {
            label(
                g,
                bx,
                y + 3.0,
                &format!("+{tail}"),
                (x + width - bx).max(0.0),
                9.5,
                theme::text_dim(),
                false,
            );
            break;
        }
    }
}

fn contains(rect: (f32, f32, f32, f32), cursor: (f32, f32)) -> bool {
    cursor.0 >= rect.0
        && cursor.0 < rect.0 + rect.2
        && cursor.1 >= rect.1
        && cursor.1 < rect.1 + rect.3
}

fn details(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    oid: &str,
    cursor: (f32, f32),
    x: f32,
    mut y: f32,
    width: f32,
    top: f32,
    bottom: f32,
) {
    let Some(files) = git.col_commit_files_cache.get(oid).cloned() else {
        label(
            g,
            x,
            y + 6.0,
            "변경 파일을 읽는 중…",
            width,
            10.5,
            theme::text_dim(),
            false,
        );
        return;
    };
    if files.is_empty() {
        label(
            g,
            x,
            y + 6.0,
            "변경 파일 없음",
            width,
            10.5,
            theme::text_dim(),
            false,
        );
        return;
    }
    for (path, add, del) in files {
        let key = (oid.to_owned(), path.clone());
        let expanded = git.col_commit_file_expanded.contains(&key);
        let row = (x, y, width, native_controls::CONTROL_HEIGHT);
        if y + row.3 > top && y < bottom {
            let ink = if expanded {
                theme::text()
            } else {
                theme::text_dim()
            };
            let hit = g.clip_hit(row);
            let hovered = hit.is_some_and(|rect| contains(rect, cursor));
            if hovered || expanded {
                g.round_rect_stroke(x, y, width, row.3, theme::radius_sm(), 1.0, theme::border());
            }
            g.queue_icon(
                if expanded {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                x,
                y + 6.0,
                14.0,
                ink,
            );
            let stat = format!("+{add} −{del}");
            let sw = g.measure_chrome_text(&stat, 10.5, false);
            label(
                g,
                x + 18.0,
                y + 6.0,
                &path,
                width - sw - 26.0,
                10.5,
                ink,
                false,
            );
            label(
                g,
                x + width - sw,
                y + 6.0,
                &stat,
                sw,
                10.5,
                theme::text_dim(),
                false,
            );
            if let Some(hit) = hit {
                git.col_commit_file_rects
                    .push((oid.to_owned(), path.clone(), hit));
            }
            g.hover_pointer |= hovered;
        }
        y += native_controls::CONTROL_HEIGHT;
        if !expanded {
            continue;
        }
        let Some(diff) = git.col_commit_diff_cache.get(&key) else {
            label(
                g,
                x + 18.0,
                y + 2.0,
                "차이를 읽는 중…",
                width - 18.0,
                10.5,
                theme::text_dim(),
                false,
            );
            y += 18.0;
            continue;
        };
        if diff.is_empty() {
            label(
                g,
                x + 18.0,
                y + 2.0,
                "표시할 텍스트 차이 없음",
                width - 18.0,
                10.5,
                theme::text_dim(),
                false,
            );
            y += 18.0;
        }
        for line in diff {
            if y + 18.0 > top && y < bottom {
                use kasa_mcp::git::DiffLineKind as K;
                let (prefix, color) = match line.kind {
                    K::Add => ("+", theme::success()),
                    K::Del => ("−", theme::danger()),
                    K::Hunk => (" ", theme::accent()),
                    K::Context => (" ", theme::text_dim()),
                };
                let color = theme::enforce_contrast_at(color, theme::panel_bg(), 4.5);
                let text = format!("{prefix}{}", line.text.trim_end());
                label(
                    g,
                    x + 18.0,
                    y + 2.0,
                    &text,
                    width - 18.0,
                    10.5,
                    color,
                    false,
                );
            }
            y += 18.0;
        }
    }
}

pub(crate) fn history(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    view: &GitColView,
    cursor: (f32, f32),
    x: f32,
    y: f32,
    width: f32,
    bottom: f32,
) {
    git.col_commit_rects.clear();
    git.col_commit_file_rects.clear();
    git.col_commits_grip = None;
    git.graph_rect = None;
    if git.branch_menu_open || view.no_repo || view.loading || view.issue.is_some() || bottom <= y {
        return;
    }
    let grip = (x, y, width, 8.0);
    git.col_commits_grip = Some(grip);
    let hot = contains(grip, cursor) || git.col_commits_resize.is_some();
    g.rect(
        x,
        y + 4.0,
        width,
        if hot { 2.0 } else { 1.0 },
        if hot {
            theme::accent()
        } else {
            theme::border()
        },
    );
    g.queue_icon(
        "git-commit-horizontal",
        x,
        y + 12.0,
        14.0,
        theme::text_dim(),
    );
    let caption = if view.graph_supported {
        "커밋 그래프"
    } else {
        "최근 커밋 · 그래프 정보 없음"
    };
    label(
        g,
        x + 22.0,
        y + 13.0,
        caption,
        width - 22.0,
        11.0,
        theme::text_dim(),
        true,
    );
    let top = (y + 34.0).min(bottom);
    let visible = (bottom - top).max(0.0);
    let commits = commits(view);
    let graph = graph::layout(&commits);
    let heights = commits
        .iter()
        .map(|commit| row_height(commit) + expanded_height(git, &commit.oid))
        .collect::<Vec<_>>();
    let tail = if view.graph_truncated || !graph.boundary.is_empty() {
        26.0
    } else {
        0.0
    };
    let content = heights.iter().sum::<f32>() + tail;
    git.graph_extent = (visible, content);
    git.graph_scroll = git.graph_scroll.clamp(0.0, (content - visible).max(0.0));
    git.graph_rect = Some((x, top, width, visible));
    g.push_clip(x, top, width, visible);
    if commits.is_empty() {
        label(
            g,
            x + 10.0,
            top + 8.0,
            if view.unborn {
                "첫 커밋을 만들면 여기에 표시돼요"
            } else {
                "커밋 기록 없음"
            },
            width - 20.0,
            12.0,
            theme::text_dim(),
            false,
        );
    }
    let graph_width = if view.graph_supported {
        graph::graph_width(graph.columns, width)
    } else {
        20.0
    };
    let tx = x + graph_width + 8.0;
    let tw = (width - graph_width - 14.0).max(0.0);
    let mut row_y = top - git.graph_scroll;
    for ((commit, row), height) in commits.iter().zip(&graph.rows).zip(heights) {
        if row_y + height > top && row_y < bottom {
            let base_h = row_height(commit);
            let rect = (tx, row_y, tw, base_h);
            let expanded = git.col_commit_expanded.as_deref() == Some(commit.oid.as_str());
            let hit = g.clip_hit(rect);
            let hovered = hit.is_some_and(|rect| contains(rect, cursor));
            if expanded || hovered {
                g.round_rect_stroke(
                    tx,
                    row_y,
                    tw,
                    base_h,
                    theme::radius_sm(),
                    1.0,
                    if expanded {
                        theme::accent()
                    } else {
                        theme::border()
                    },
                );
            }
            if view.graph_supported {
                rails(g, row, graph.columns, x, row_y, graph_width, height);
            } else {
                g.queue_icon(
                    "git-commit-horizontal",
                    x + 3.0,
                    row_y + 6.0,
                    14.0,
                    theme::text_dim(),
                );
            }
            label(
                g,
                tx + 6.0,
                row_y + 4.0,
                &commit.subject,
                tw - 24.0,
                12.0,
                theme::text(),
                false,
            );
            g.queue_icon(
                if expanded {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                tx + tw - 16.0,
                row_y + 5.0,
                14.0,
                theme::text_dim(),
            );
            let metadata = if commit.author.is_empty() {
                short_oid(&commit.oid)
            } else {
                format!("{}  {}", short_oid(&commit.oid), commit.author)
            };
            label(
                g,
                tx + 6.0,
                row_y + 22.0,
                &metadata,
                tw - 12.0,
                10.5,
                theme::text_dim(),
                false,
            );
            if !commit.refs.is_empty() {
                refs(g, &commit.refs, tx + 6.0, row_y + 38.0, tw - 12.0);
            }
            if let Some(hit) = hit {
                git.col_commit_rects.push((commit.oid.clone(), hit));
            }
            g.hover_pointer |= hovered;
            if expanded {
                details(
                    g,
                    git,
                    &commit.oid,
                    cursor,
                    tx,
                    row_y + base_h,
                    tw,
                    top,
                    bottom,
                );
            }
        }
        row_y += height;
    }
    if tail > 0.0 {
        label(
            g,
            tx,
            row_y + 6.0,
            "이전 기록은 조회 범위 밖이에요",
            width - graph_width - 8.0,
            10.5,
            theme::text_dim(),
            false,
        );
    }
    g.pop_clip();
    if content > visible && visible > 0.0 {
        let thumb_h = (visible * visible / content).max(18.0).min(visible);
        let thumb_y = top + (visible - thumb_h) * git.graph_scroll / (content - visible);
        g.round_rect_fill(
            x + width - 3.0,
            thumb_y,
            3.0,
            thumb_h,
            1.5,
            theme::text_dim(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refs_keep_local_and_remote_identity_distinct() {
        assert_eq!(
            ref_label("refs/heads/origin/main"),
            ("git-branch", "origin/main")
        );
        assert_eq!(
            ref_label("refs/remotes/origin/main"),
            ("server", "origin/main")
        );
        assert_eq!(ref_label("HEAD"), ("check", "HEAD"));
    }
    #[test]
    fn unavailable_graph_does_not_invent_parent_relationships() {
        let view = GitColView {
            recent_commits: vec![("abc".into(), "one".into()), ("def".into(), "two".into())],
            ..Default::default()
        };
        assert!(commits(&view)
            .iter()
            .all(|commit| commit.parents.is_empty()));
    }
    #[test]
    fn history_height_respects_pinned_size_and_zero_room() {
        let view = GitColView {
            graph_supported: true,
            ..Default::default()
        };
        let mut state = state::GitState::default();
        assert_eq!(history_height(&view, &state, 0.0), 0.0);
        state.col_commits_h = Some(600.0);
        assert_eq!(history_height(&view, &state, 120.0), 120.0);
        assert_eq!(requested_history_count(120.0), 200);
    }
}
