use super::{harness_color, harness_icon, inside, model, Action};
use crate::{gpu, info, native_controls as controls, sprites, state, theme, ClosedPane};
type Rect = (f32, f32, f32, f32);
const ROW_H: f32 = 80.0;

fn text(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, value: &str, size: f32, dim: bool) {
    let ink = theme::enforce_contrast_at(
        if dim {
            theme::text_dim()
        } else {
            theme::text()
        },
        theme::sidebar_bg(),
        4.5,
    );
    let shown = info::fit_text(g, value, w.max(0.0), size, false);
    g.draw_text(
        x,
        y,
        &shown,
        gpu::DrawOpts {
            font_size: size,
            color: ink,
            bold: false,
            italic: false,
        },
    );
}

fn register(
    g: &mut gpu::GpuRenderer,
    sc: &mut state::SessionsColState,
    action: Action,
    rect: Rect,
) {
    if g.clip_hit(rect)
        .is_some_and(|clipped| fully_visible(rect, clipped))
    {
        if sc.keyboard_focus.as_ref() == Some(&action) {
            controls::focus_ring(g, rect);
        }
        sc.actions.push((action, rect));
    }
}

fn fully_visible(rect: Rect, clipped: Rect) -> bool {
    // Clip intersection recomputes widths; fractional glyph advances must remain clickable.
    (rect.0 - clipped.0).abs() < 0.01
        && (rect.1 - clipped.1).abs() < 0.01
        && (rect.2 - clipped.2).abs() < 0.01
        && (rect.3 - clipped.3).abs() < 0.01
}

fn button(
    g: &mut gpu::GpuRenderer,
    sc: &mut state::SessionsColState,
    cursor: (f32, f32),
    rect: Rect,
    label: &str,
    action: Action,
    primary: bool,
    danger: bool,
) -> Rect {
    let rect = controls::text_button(
        g,
        rect,
        cursor,
        label,
        controls::Style {
            primary,
            danger,
            ..Default::default()
        },
    );
    register(g, sc, action, rect);
    rect
}

pub(super) fn flow(widths: &[f32], available: f32) -> (Vec<Rect>, f32) {
    let mut x = 0.0;
    let mut y = 0.0;
    let available = available.max(0.0);
    let mut rects = Vec::new();
    for width in widths {
        let width = width.min(available);
        if x > 0.0 && x + width > available {
            x = 0.0;
            y += 34.0;
        }
        rects.push((x, y, width, controls::CONTROL_HEIGHT));
        x += width + 8.0;
    }
    (rects, y + controls::CONTROL_HEIGHT)
}

fn choices(
    g: &mut gpu::GpuRenderer,
    sc: &mut state::SessionsColState,
    cursor: (f32, f32),
    x: f32,
    y: f32,
    w: f32,
    items: Vec<(String, Action, bool)>,
) -> f32 {
    let widths: Vec<_> = items
        .iter()
        .map(|(s, _, _)| g.measure_chrome_text(s, 12.0, true) + 20.0)
        .collect();
    let (rects, height) = flow(&widths, w);
    for ((label, action, primary), r) in items.into_iter().zip(rects) {
        button(
            g,
            sc,
            cursor,
            (x + r.0, y + r.1, r.2, r.3),
            &label,
            action,
            primary,
            false,
        );
    }
    height
}

fn lines(g: &mut gpu::GpuRenderer, value: &str, width: f32, max: usize) -> Vec<String> {
    let mut result =
        crate::native_settings::wrap_words(g, &model::one_line(value), width.max(0.0), 12.0);
    if result.len() > max {
        let tail = result[max.saturating_sub(1)..].join(" ");
        result.truncate(max);
        if let Some(last) = result.last_mut() {
            *last = info::fit_text(g, &tail, width, 12.0, false);
        }
    }
    result
}

fn identity(g: &mut gpu::GpuRenderer, student: &str, harness: &str, x: f32, y: f32) {
    if student.is_empty() || !sprites::draw_student_face(g, student, x, y, 26.0) {
        g.round_rect_fill(x, y, 26.0, 26.0, theme::radius_sm(), theme::surface());
        g.queue_icon(
            if harness.is_empty() {
                "terminal"
            } else {
                harness_icon(harness)
            },
            x + 5.0,
            y + 5.0,
            16.0,
            if harness.is_empty() {
                theme::text_dim()
            } else {
                harness_color(harness)
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_sessions_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    sc: &mut state::SessionsColState,
    closed: &[ClosedPane],
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
    _frozen_content: Option<f32>,
) {
    sc.actions.clear();
    sc.closed_rects.clear();
    sc.scroll_track = None;
    g.push_clip(x, top, w.max(0.0), (bottom - top).max(0.0));
    let left = x + 14.0;
    let width = (w - 28.0).max(0.0);
    let mut y = top + 10.0;
    text(g, left, y, width - 34.0, "대화 기록", 14.0, false);
    let refresh = controls::icon_button(
        g,
        (x + w - 40.0, y - 4.0, 26.0, 26.0),
        cursor,
        "rotate-cw",
        Default::default(),
    );
    register(g, sc, Action::Refresh, refresh);
    y += 24.0;
    text(
        g,
        left,
        y,
        width,
        "이 기기에 저장된 대화를 다시 찾아요",
        10.5,
        true,
    );
    y += 24.0;
    y += choices(
        g,
        sc,
        cursor,
        left,
        y,
        width,
        vec![
            ("대화".into(), Action::Mode(false), !sc.retained_mode),
            (
                format!("닫은 칸 {}", closed.len()),
                Action::Mode(true),
                sc.retained_mode,
            ),
        ],
    ) + 12.0;
    if !sc.retained_mode {
        y += choices(
            g,
            sc,
            cursor,
            left,
            y,
            width,
            vec![
                ("현재 폴더".into(), Action::Scope(false), !sc.scope_all),
                ("이 기기 전체".into(), Action::Scope(true), sc.scope_all),
            ],
        ) + 8.0;
        let filters = [
            (None, "모두"),
            (Some("claude"), "Claude"),
            (Some("codex"), "Codex"),
            (Some("agy"), "Agy"),
        ];
        y += choices(
            g,
            sc,
            cursor,
            left,
            y,
            width,
            filters
                .into_iter()
                .map(|(kind, label)| {
                    (
                        label.into(),
                        Action::Filter(kind.map(str::to_string)),
                        sc.filter.as_deref() == kind,
                    )
                })
                .collect(),
        ) + 10.0;
        let count = sc
            .view
            .iter()
            .filter(|s| sc.filter.as_deref().is_none_or(|f| f == s.harness))
            .count();
        let scope = if sc.scope_all {
            "모든 폴더".to_owned()
        } else {
            sc.cwd
                .as_ref()
                .map(|p| model::folder(&p.to_string_lossy()).to_owned())
                .unwrap_or("폴더 미확인".into())
        };
        text(
            g,
            left,
            y,
            width,
            &format!("{scope} · {count}개"),
            10.5,
            true,
        );
    } else {
        text(
            g,
            left,
            y,
            width,
            "닫거나 보관한 칸 · 실행 상태는 각각 달라요",
            10.5,
            true,
        );
    }
    y += 26.0;
    let body_top = y.min(bottom);
    let body_h = (bottom - body_top - 8.0).max(0.0);
    sc.body_rect = (x, body_top, w, body_h);
    g.push_clip(x, body_top, w, body_h);
    let text_x = left + 36.0;
    let text_w = (width - 46.0).max(0.0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let items: Vec<_> = sc
        .view
        .iter()
        .filter(|s| sc.filter.as_deref().is_none_or(|f| f == s.harness))
        .cloned()
        .collect();
    let selected_lines = items
        .iter()
        .find(|s| sc.selected.as_ref() == Some(&model::key(s)))
        .map(|s| {
            (
                lines(
                    g,
                    if s.preview.is_empty() {
                        "대화 미리보기가 없어요. 저장된 대화는 이어갈 수 있어요."
                    } else {
                        &s.preview
                    },
                    width - 20.0,
                    5,
                ),
                lines(
                    g,
                    if s.cwd.is_empty() {
                        "작업 폴더 미확인"
                    } else {
                        &s.cwd
                    },
                    width - 20.0,
                    2,
                ),
            )
        });
    let detail_h = selected_lines
        .as_ref()
        .map_or(0.0, |(a, b)| (a.len() + b.len()) as f32 * 18.0 + 62.0);
    sc.content_h = if sc.retained_mode {
        closed
            .iter()
            .map(|c| {
                ROW_H
                    + if sc.selected_closed.as_deref() == Some(&model::closed_key(c)) {
                        80.0
                    } else {
                        0.0
                    }
            })
            .sum()
    } else {
        items.len() as f32 * ROW_H + detail_h
    };
    sc.scroll = sc.scroll.clamp(0.0, (sc.content_h - body_h).max(0.0));
    y = body_top - sc.scroll;
    let empty = if sc.retained_mode {
        closed.is_empty()
    } else {
        items.is_empty()
    };
    if empty {
        g.queue_icon(
            if sc.retained_mode {
                "terminal"
            } else {
                "rotate-cw"
            },
            left,
            y + 18.0,
            26.0,
            theme::text_dim(),
        );
        let loading = !sc.retained_mode && sc.busy.load(std::sync::atomic::Ordering::Relaxed);
        text(
            g,
            left,
            y + 58.0,
            width,
            if loading {
                "대화 기록을 읽는 중…"
            } else if sc.retained_mode {
                "닫은 칸이 없어요"
            } else {
                "이 범위에 대화가 없어요"
            },
            12.0,
            false,
        );
        text(
            g,
            left,
            y + 82.0,
            width,
            if sc.retained_mode {
                "다시 열 수 있는 칸이 여기에 남아요"
            } else {
                "위에서 폴더 범위나 대화 종류를 바꿔 보세요"
            },
            10.5,
            true,
        );
    }
    if sc.retained_mode {
        for (i, c) in closed.iter().enumerate().rev() {
            let key = model::closed_key(c);
            let selected = sc.selected_closed.as_ref() == Some(&key);
            let row_h = ROW_H + if selected { 80.0 } else { 0.0 };
            if g.clip_visible(left, y, width, row_h) {
                row_background(g, cursor, (left, y, width, row_h - 8.0), selected);
                let harness = c.rec["was_agent"].as_str().unwrap_or("");
                let student = if model::has_agent(&c.rec) {
                    c.character.as_str()
                } else {
                    ""
                };
                if c.preview.is_some() {
                    g.queue_icon("file", left + 12.0, y + 14.0, 20.0, theme::text_dim());
                } else {
                    identity(g, student, harness, left + 8.0, y + 10.0);
                }
                let title = c
                    .preview
                    .as_ref()
                    .map(|(_, path)| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .or_else(|| {
                        c.rec["title"]
                            .as_str()
                            .filter(|s| !s.trim().is_empty())
                            .map(model::one_line)
                    })
                    .unwrap_or_else(|| {
                        if student.is_empty() {
                            format!(
                                "{}의 셸",
                                if c.folder.is_empty() {
                                    "이전 폴더"
                                } else {
                                    &c.folder
                                }
                            )
                        } else {
                            format!("{student}의 대화")
                        }
                    });
                text(g, text_x + 8.0, y + 10.0, text_w - 8.0, &title, 12.0, false);
                text(
                    g,
                    text_x + 8.0,
                    y + 32.0,
                    text_w - 8.0,
                    &format!("{} · {}", c.folder, c.pane_id),
                    10.5,
                    true,
                );
                text(
                    g,
                    text_x + 8.0,
                    y + 53.0,
                    text_w - 8.0,
                    &format!(
                        "{} · {}",
                        c.retention_label().unwrap_or("닫은 칸"),
                        if c.alive { "실행 중" } else { "종료됨" }
                    ),
                    10.5,
                    true,
                );
                if let Some(hit) = g.clip_hit((left, y, width, ROW_H - 8.0)) {
                    register(g, sc, Action::ClosedSelect(key.clone()), hit);
                }
                if selected {
                    let confirm = sc.confirm_discard.as_ref() == Some(&key);
                    let label = if c.alive {
                        "실행 종료"
                    } else {
                        "목록에서 지우기"
                    };
                    let hint = if confirm {
                        if c.alive {
                            "이 칸의 실행도 끝낼까요?"
                        } else {
                            "이 목록에서만 지워요. 대화 파일은 남아요."
                        }
                    } else if c.alive {
                        "다시 열면 실행하던 화면으로 돌아가요"
                    } else {
                        "저장된 기록으로 칸을 다시 열어요"
                    };
                    text(g, left + 10.0, y + ROW_H, width - 20.0, hint, 10.5, true);
                    let bw = ((width - 28.0) / 2.0).max(0.0);
                    let first = if confirm {
                        Action::CancelDiscard
                    } else {
                        Action::Reopen(key.clone())
                    };
                    let br = button(
                        g,
                        sc,
                        cursor,
                        (left + 10.0, y + ROW_H + 30.0, bw, 26.0),
                        if confirm { "취소" } else { "다시 열기" },
                        first,
                        !confirm,
                        false,
                    );
                    if !confirm {
                        sc.closed_rects.push((i, br));
                    }
                    button(
                        g,
                        sc,
                        cursor,
                        (left + 18.0 + bw, y + ROW_H + 30.0, bw, 26.0),
                        label,
                        if confirm {
                            Action::Discard(key.clone())
                        } else {
                            Action::AskDiscard(key.clone())
                        },
                        false,
                        true,
                    );
                }
            }
            y += row_h;
        }
    } else {
        for s in &items {
            let selected = sc.selected.as_ref() == Some(&model::key(s));
            let row_h = ROW_H + if selected { detail_h } else { 0.0 };
            if g.clip_visible(left, y, width, row_h) {
                row_background(g, cursor, (left, y, width, row_h - 8.0), selected);
                identity(g, &s.student, &s.harness, left + 8.0, y + 10.0);
                text(
                    g,
                    text_x + 8.0,
                    y + 10.0,
                    text_w - 8.0,
                    &model::title(s),
                    12.0,
                    false,
                );
                let preview = if selected {
                    format!(
                        "{}{}",
                        model::harness_name(&s.harness),
                        if s.student.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", s.student)
                        }
                    )
                } else if s.preview.is_empty() {
                    "미리보기 없음 · 선택해서 확인".into()
                } else {
                    model::one_line(&s.preview)
                };
                text(
                    g,
                    text_x + 8.0,
                    y + 32.0,
                    text_w - 8.0,
                    &preview,
                    10.5,
                    true,
                );
                text(
                    g,
                    text_x + 8.0,
                    y + 53.0,
                    text_w - 8.0,
                    &format!(
                        "{} · {}",
                        model::folder(&s.cwd),
                        model::ago_at(s.mtime, now)
                    ),
                    10.5,
                    true,
                );
                if let Some(hit) = g.clip_hit((left, y, width, ROW_H - 8.0)) {
                    register(g, sc, Action::Select(s.harness.clone(), s.id.clone()), hit);
                }
                if selected {
                    let mut dy = y + ROW_H;
                    if let Some((preview, path)) = &selected_lines {
                        for line in preview {
                            text(g, left + 10.0, dy, width - 20.0, line, 12.0, false);
                            dy += 18.0;
                        }
                        dy += 12.0;
                        for line in path {
                            text(g, left + 10.0, dy, width - 20.0, line, 10.5, true);
                            dy += 18.0;
                        }
                    }
                    button(
                        g,
                        sc,
                        cursor,
                        (left + 10.0, dy + 10.0, width - 20.0, 26.0),
                        "새 방에서 이어가기",
                        Action::Resume(s.harness.clone(), s.id.clone()),
                        true,
                        false,
                    );
                }
            }
            y += row_h;
        }
    }
    g.pop_clip();
    if let Some((ty, thumb_h)) = model::scroll_thumb(body_top, body_h, sc.content_h, sc.scroll) {
        sc.scroll_track = Some((x + w - 10.0, body_top, 10.0, body_h));
        g.round_rect_fill(x + w - 6.0, ty, 4.0, thumb_h, 2.0, theme::text_dim());
    }
    g.pop_clip();
}

fn row_background(g: &mut gpu::GpuRenderer, cursor: (f32, f32), rect: Rect, selected: bool) {
    let hover = g.clip_hit(rect).is_some_and(|r| inside(cursor, &r));
    g.hover_pointer |= hover;
    let fill = if selected {
        theme::surface_active()
    } else if hover {
        theme::surface_hover()
    } else {
        theme::surface()
    };
    g.round_rect_fill(rect.0, rect.1, rect.2, rect.3, theme::radius_sm(), fill);
    if selected {
        g.round_rect_stroke(
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            theme::radius_sm(),
            1.0,
            theme::accent(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filter_controls_wrap_without_overlap_or_crossing_the_panel() {
        for width in [160.0, 240.0, 320.0, 640.0] {
            let (rects, height) = flow(&[64.0, 90.0, 86.0, 60.0], width);
            for (i, r) in rects.iter().enumerate() {
                assert!(r.0 >= 0.0 && r.0 + r.2 <= width && r.1 + r.3 <= height);
                for q in rects.iter().skip(i + 1) {
                    assert!(
                        r.0 + r.2 <= q.0
                            || q.0 + q.2 <= r.0
                            || r.1 + r.3 <= q.1
                            || q.1 + q.3 <= r.1
                    );
                }
            }
        }
    }
    #[test]
    fn fractional_button_widths_survive_clip_rounding_but_not_real_clipping() {
        let r = (1014.0, 20.0, 68.1, 26.0);
        assert!(fully_visible(r, (r.0, r.1, (r.0 + r.2) - r.0, r.3)));
        assert!(!fully_visible(r, (r.0, r.1, r.2 - 0.5, r.3)));
    }
}
