use crate::{gpu, info, native_controls as controls, sprites, theme, App, RestoreBtn};
type Rect = (f32, f32, f32, f32);

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub title: String,
    pub detail: String,
    pub character: String,
    pub icon: &'static str,
}

impl Item {
    pub(super) fn from_record(record: &serde_json::Value, agent: bool) -> Self {
        let string = |key: &str| record[key].as_str().unwrap_or("").trim();
        let character = if agent { string("character") } else { "" };
        let kind = super::restore::saved_agent_marker(record);
        let (name, icon) = match kind {
            Some("codex") => ("Codex", "codex"),
            Some("agy") => ("Antigravity", "antigravity"),
            _ if agent => ("Claude", "claude"),
            _ if record.get("web_url").is_some() => ("웹 페이지", "globe"),
            _ => ("일반 셸", "terminal"),
        };
        let cwd = string("cwd");
        let folder = cwd
            .rsplit(['/', '\\'])
            .find(|s| !s.is_empty())
            .unwrap_or("폴더 미확인");
        let title = if !string("title").is_empty() {
            string("title")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        } else if character.is_empty() {
            format!("{folder} · {name}")
        } else {
            format!("{character}의 {name} 작업")
        };
        let detail = format!(
            "{name}{} · {folder}",
            if character.is_empty() {
                String::new()
            } else {
                format!(" · {character}")
            }
        );
        Self {
            title,
            detail,
            character: character.into(),
            icon,
        }
    }
}

pub(crate) struct Layout {
    pub card: Rect,
    pub close: Rect,
    pub restore: Rect,
    pub fresh: Rect,
    pub list: Rect,
    pub rows: usize,
    pub pad: f32,
    pub note: bool,
}

pub(crate) fn layout(width: f32, height: f32, count: usize) -> Layout {
    let width = width.max(0.0);
    let height = height.max(0.0);
    let w = 620.0_f32.min((width - 32.0).max(0.0));
    let pad = if w < 420.0 { 16.0_f32 } else { 24.0 }.min(w / 6.0);
    let max_h = (height - 32.0).max(0.0);
    let note = max_h >= 320.0;
    let base = if note { 232.0 } else { 176.0 };
    let rows = count
        .min(4)
        .min(((max_h - base).max(0.0) / 56.0).floor() as usize);
    let h = (base + rows as f32 * 56.0).min(max_h);
    let x = ((width - w) / 2.0).round();
    let y = ((height - h) / 2.0).round();
    let button_h = 26.0_f32.min((h / 3.0).max(0.0));
    let bw = ((w - 2.0 * pad - 8.0) / 2.0).max(0.0).min(172.0);
    let by = y + h - pad - button_h;
    Layout {
        card: (x, y, w, h),
        close: (x + w - pad - 26.0, y + pad, 26.0, 26.0),
        restore: (x + w - pad - bw, by, bw, button_h),
        fresh: (x + w - pad - bw * 2.0 - 8.0, by, bw, button_h),
        list: (
            x + pad,
            y + 112.0,
            (w - pad * 2.0).max(0.0),
            rows as f32 * 56.0,
        ),
        rows,
        pad,
        note,
    }
}

fn label(g: &mut gpu::GpuRenderer, rect: Rect, value: &str, size: f32, dim: bool) {
    let value = info::fit_text(g, value, rect.2.max(0.0), size, false);
    g.draw_text(
        rect.0,
        rect.1,
        &value,
        gpu::DrawOpts {
            font_size: size,
            bold: false,
            italic: false,
            color: theme::enforce_contrast_at(
                if dim {
                    theme::text_dim()
                } else {
                    theme::text()
                },
                theme::panel_bg(),
                4.5,
            ),
        },
    );
}

pub(crate) fn paint(
    g: &mut gpu::GpuRenderer,
    state: &serde_json::Value,
    size: (f32, f32),
    cursor: (f32, f32),
    focus: Option<RestoreBtn>,
) -> Vec<(RestoreBtn, Rect)> {
    let summary = App::restore_summary(state);
    let l = layout(size.0, size.1, summary.items.len());
    let (x, y, w, h) = l.card;
    g.rect(
        0.0,
        0.0,
        size.0,
        size.1,
        theme::with_alpha([0, 0, 0, 255], 110),
    );
    crate::panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::panel_bg());
    g.push_clip(x, y, w, h);
    label(
        g,
        (x + l.pad, y + l.pad, w - l.pad * 2.0 - 38.0, 26.0),
        "작업을 이어서 열까요?",
        20.0,
        false,
    );
    label(
        g,
        (x + l.pad, y + l.pad + 34.0, w - l.pad * 2.0, 18.0),
        &format!(
            "이 기기 · {}",
            crate::restore_progress::last_used_label(state)
        ),
        12.0,
        true,
    );
    label(
        g,
        (x + l.pad, y + 86.0, w - l.pad * 2.0, 18.0),
        &format!(
            "칸 {}개   학생 {}명   셸 {}개",
            summary.panes, summary.agents, summary.shells
        ),
        12.0,
        false,
    );
    for (i, item) in summary.items.iter().take(l.rows).enumerate() {
        let ry = l.list.1 + i as f32 * 56.0;
        g.round_rect_fill(
            l.list.0,
            ry,
            l.list.2,
            50.0,
            theme::radius_sm(),
            theme::surface(),
        );
        let fx = l.list.0 + 10.0;
        if item.character.is_empty()
            || !sprites::draw_student_face(g, &item.character, fx, ry + 12.0, 26.0)
        {
            g.queue_icon(item.icon, fx + 3.0, ry + 15.0, 20.0, theme::text_dim());
        }
        label(
            g,
            (fx + 38.0, ry + 8.0, l.list.2 - 58.0, 18.0),
            &item.title,
            12.0,
            false,
        );
        label(
            g,
            (fx + 38.0, ry + 29.0, l.list.2 - 58.0, 16.0),
            &item.detail,
            10.5,
            true,
        );
    }
    let extra = summary.items.len().saturating_sub(l.rows);
    let after = l.list.1 + l.list.3 + 8.0;
    if extra > 0 && after + 18.0 < l.fresh.1 {
        label(
            g,
            (l.list.0, after, l.list.2, 18.0),
            &format!("외 {extra}개 작업도 함께 복원해요"),
            10.5,
            true,
        );
    }
    if l.note {
        label(
            g,
            (l.list.0, after + 24.0, l.list.2, 18.0),
            "다른 기기의 작업은 복원하지 않아요.",
            10.5,
            true,
        );
        label(
            g,
            (l.list.0, after + 42.0, l.list.2, 18.0),
            "연결되면 왼쪽 기기 목록에 나타나요.",
            10.5,
            true,
        );
    }
    let close = controls::icon_button(g, l.close, cursor, "x", Default::default());
    let fresh = controls::text_button(g, l.fresh, cursor, "새로 시작", Default::default());
    let restore = controls::text_button(
        g,
        l.restore,
        cursor,
        "이전 작업 복원",
        controls::Style {
            primary: true,
            ..Default::default()
        },
    );
    let hits = vec![
        (RestoreBtn::Restore, restore),
        (RestoreBtn::Fresh, fresh),
        (RestoreBtn::Dismiss, close),
    ];
    for (button, r) in &hits {
        if focus == Some(*button) {
            controls::focus_ring(g, *r);
        }
    }
    g.pop_clip();
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dialog_actions_stay_inside_short_narrow_and_wide_windows() {
        for (w, h) in [
            (280.0, 200.0),
            (390.0, 360.0),
            (640.0, 540.0),
            (1100.0, 760.0),
            (1920.0, 1080.0),
        ] {
            for count in [0, 1, 8, 100] {
                let l = layout(w, h, count);
                for r in [l.card, l.close, l.restore, l.fresh] {
                    assert!(r.0 >= 0.0 && r.1 >= 0.0 && r.0 + r.2 <= w && r.1 + r.3 <= h);
                }
                assert!(l.fresh.0 + l.fresh.2 < l.restore.0);
                assert!(l.list.1 + l.list.3 <= l.fresh.1);
                assert!(l.rows <= count && l.rows <= 4);
            }
        }
    }
    #[test]
    fn plain_shell_never_uses_its_old_student_face() {
        let item = Item::from_record(
            &serde_json::json!({"character":"이전 학생","cwd":"C:\\work\\프로젝트"}),
            false,
        );
        assert!(item.character.is_empty());
        assert_eq!(item.icon, "terminal");
        assert!(item.title.contains("프로젝트"));
    }
}
