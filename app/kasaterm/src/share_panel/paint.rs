use super::*;

fn text(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    width: f32,
    label: &str,
    font: f32,
    color: [u8; 4],
    bold: bool,
) {
    let shown = crate::info::fit_text(g, label, width.max(0.0), font, bold);
    g.draw_text(
        x,
        y,
        &shown,
        gpu::DrawOpts {
            font_size: font,
            color,
            bold,
            italic: false,
        },
    );
}

fn button(
    g: &mut gpu::GpuRenderer,
    state: &mut State,
    cursor: (f32, f32),
    rect: Rect,
    label: &str,
    action: Action,
    active: bool,
) {
    let rect = native_controls::text_button(
        g,
        rect,
        cursor,
        label,
        native_controls::Style {
            active,
            ..Default::default()
        },
    );
    if state.keyboard_focus.as_ref() == Some(&action) {
        native_controls::focus_ring(g, rect);
    }
    if let Some(rect) = g.clip_hit(rect) {
        state.hits.push((action, rect));
    }
}

#[derive(Debug)]
struct ContentLayout {
    list: Rect,
    preview: Option<Rect>,
    footer: f32,
}

fn content_layout(x: f32, y: f32, width: f32, bottom: f32, selected: bool) -> ContentLayout {
    let footer = (bottom - 72.0).max(y);
    let height = (footer - y).max(0.0);
    if selected && width >= 560.0 {
        let preview_width = (width * 0.46).min(440.0);
        let list_width = (width - preview_width - 16.0).max(0.0);
        ContentLayout { list: (x, y, list_width, height),
            preview: Some((x + list_width + 16.0, y, preview_width, height)), footer }
    } else {
        let preview_height = if selected { (height * 0.48).min(240.0) } else { 0.0 };
        ContentLayout { list: (x, y, width, height - preview_height),
            preview: selected.then_some((x, footer - preview_height, width, preview_height)), footer }
    }
}

pub(crate) fn draw(
    g: &mut gpu::GpuRenderer,
    state: &mut State,
    cursor: (f32, f32),
    preedit: &str,
    x: f32,
    width: f32,
    top: f32,
    bottom: f32,
) {
    state.hits.clear();
    state.search_rect = None;
    state.caret_rect = None;
    let left = x + 14.0;
    let w = (width - 28.0).max(1.0);
    let mut y = top + 8.0;
    g.push_clip(x, top, width, (bottom - top).max(0.0));
    let search = (left, y, w, 26.0);
    g.round_rect_fill(
        search.0,
        search.1,
        search.2,
        search.3,
        theme::radius_sm(),
        theme::surface_hover(),
    );
    g.queue_icon("search", left + 7.0, y + 6.0, 14.0, theme::text_dim());
    let label = if state.query.is_empty() && preedit.is_empty() {
        "이름·종류·기기 검색…".to_owned()
    } else {
        let (before, after) = lineedit::split(&state.query, state.caret);
        format!("{before}{preedit}{after}")
    };
    if state.search_focus && state.select_all && !state.query.is_empty() {
        let selected_width = g
            .measure_chrome_text(&state.query, 12.0, false)
            .min((w - 57.0).max(0.0));
        g.rect(
            left + 27.0,
            y + 4.0,
            selected_width,
            18.0,
            theme::with_alpha(theme::accent(), 60),
        );
    }
    text(
        g,
        left + 27.0,
        y + 6.0,
        w - 57.0,
        &label,
        12.0,
        if state.query.is_empty() {
            theme::text_dim()
        } else {
            theme::text()
        },
        false,
    );
    if state.search_focus {
        native_controls::focus_ring(g, search);
        let (before, _) = lineedit::split(&state.query, state.caret);
        let caret =
            (left + 27.0 + g.measure_chrome_text(&format!("{before}{preedit}"), 12.0, false))
                .min(left + w - 30.0);
        g.rect(caret, y + 5.0, 1.0, 16.0, theme::text());
        state.caret_rect = Some((caret, y + 5.0, 1.0, 16.0));
    }
    state.search_rect = g.clip_hit(search);
    if let Some(rect) = state.search_rect {
        state.hits.push((Action::Search, rect));
    }
    if !state.query.is_empty() {
        let rect = native_controls::icon_button(
            g,
            (left + w - 26.0, y, 26.0, 26.0),
            cursor,
            "x",
            Default::default(),
        );
        if let Some(rect) = g.clip_hit(rect) {
            state.hits.push((Action::ClearSearch, rect));
        }
    }
    y += 34.0;
    let recent = state.view == View::Recent;
    let folders = state.view == View::Folders;
    let mode_w = ((w - 70.0) * 0.5).clamp(44.0, 64.0);
    button(
        g,
        state,
        cursor,
        (left, y, mode_w, 26.0),
        "최근",
        Action::Recent,
        recent,
    );
    button(
        g,
        state,
        cursor,
        (left + mode_w + 6.0, y, mode_w, 26.0),
        "폴더",
        Action::Folders,
        folders,
    );
    if w < 190.0 { y += 34.0; }
    for (offset, icon, action) in [
        (0.0, "settings-2", Action::Settings),
        (32.0, "rotate-cw", Action::Refresh),
    ] {
        let rect = native_controls::icon_button(
            g,
            (left + w - 26.0 - offset, y, 26.0, 26.0),
            cursor,
            icon,
            Default::default(),
        );
        if let Some(rect) = g.clip_hit(rect) {
            if state.keyboard_focus.as_ref() == Some(&action) {
                native_controls::focus_ring(g, rect);
            }
            state.hits.push((action, rect));
        }
    }
    y += 34.0;
    if state.view == View::Folders && !state.directory.is_empty() && state.query.is_empty() {
        let rect = native_controls::icon_button(
            g,
            (left, y, 26.0, 26.0),
            cursor,
            "arrow-left",
            Default::default(),
        );
        if let Some(rect) = g.clip_hit(rect) {
            state.hits.push((Action::Back, rect));
        }
        button(
            g,
            state,
            cursor,
            (left + 32.0, y, 50.0, 26.0),
            "처음",
            Action::Root,
            false,
        );
        text(
            g,
            left + 90.0,
            y + 7.0,
            w - 90.0,
            &state.directory,
            10.5,
            theme::text_dim(),
            false,
        );
        y += 34.0;
    } else {
        text(
            g,
            left,
            y + 3.0,
            w,
            if !state.query.is_empty() {
                "모든 공유 파일에서 검색"
            } else if state.view == View::Recent {
                "최근 변경된 파일"
            } else {
                "KASA-share"
            },
            10.5,
            theme::text_dim(),
            false,
        );
        y += 24.0;
    }
    let layout = content_layout(left, y, w, bottom, state.selected.is_some());
    let footer_y = layout.footer;
    let list_h = layout.list.3;
    let list_w = layout.list.2;
    state.scroll_max = (state.rows.len() as f32 * 40.0 - list_h).max(0.0);
    state.scroll = state.scroll.clamp(0.0, state.scroll_max);
    state.scroll_track = (state.scroll_max > 0.0).then_some((left + list_w - 5.0, y, 5.0, list_h));
    g.push_clip(left, y, list_w, list_h);
    if let Some(problem) = state.problem.as_deref() {
        text(
            g,
            left,
            y + 12.0,
            list_w,
            problem,
            12.0,
            theme::text_dim(),
            false,
        );
    } else if !state.ready {
        text(
            g,
            left,
            y + 12.0,
            list_w,
            "공유 파일을 읽는 중…",
            12.0,
            theme::text_dim(),
            false,
        );
    } else if state.rows.is_empty() {
        let empty = if state.query.is_empty() {
            "공유 폴더에 파일을 넣으면 여기에 보여요"
        } else {
            "맞는 파일이 없어요. 검색어를 바꿔 주세요"
        };
        text(g, left, y + 12.0, list_w, empty, 12.0, theme::text_dim(), false);
    } else {
        let start = (state.scroll / 40.0).floor() as usize;
        let end = (start + (list_h / 40.0).ceil() as usize + 1).min(state.rows.len());
        for index in start..end {
            let row = &state.rows[index];
            let ry = y + index as f32 * 40.0 - state.scroll;
            let rect = (left, ry, list_w, 40.0);
            let hovered = contains(rect, cursor);
            let selected = state.selected.as_deref() == Some(&row.path);
            if hovered || selected {
                g.round_rect_fill(
                    left,
                    ry,
                    list_w,
                    40.0,
                    theme::radius_sm(),
                    if selected { theme::lerp(theme::surface(), theme::accent(), 0.14) } else { theme::surface_hover() },
                );
            }
            let icon = if row.folder {
                "folder"
            } else {
                match row.kind.as_str() {
                    "image" => "image",
                    "video" => "play",
                    _ => "file-text",
                }
            };
            g.round_rect_fill(left + 4.0, ry + 5.0, 30.0, 30.0, theme::radius_sm(),
                theme::lerp(theme::surface(), theme::accent(), 0.10));
            g.queue_icon(icon, left + 11.0, ry + 12.0, 16.0,
                theme::enforce_contrast_at(theme::accent(), theme::surface(), 4.5));
            text(
                g,
                left + 44.0,
                ry + 4.0,
                list_w - 50.0,
                &row.title,
                12.0,
                theme::text(),
                selected,
            );
            text(
                g,
                left + 44.0,
                ry + 23.0,
                list_w - 50.0,
                &row.detail,
                10.5,
                theme::text_dim(),
                false,
            );
            if let Some(rect) = g.clip_hit(rect) {
                let action = if row.folder {
                    Action::Folder(row.path.clone())
                } else {
                    Action::Preview(row.path.clone())
                };
                if state.keyboard_focus.as_ref() == Some(&action) {
                    native_controls::focus_ring(g, rect);
                }
                state.hits.push((
                    if row.folder {
                        Action::Folder(row.path.clone())
                    } else {
                        Action::Preview(row.path.clone())
                    },
                    rect,
                ));
            }
            g.hover_pointer |= hovered;
        }
    }
    g.pop_clip();
    if let Some((x, y, w, h)) = state.thumb() {
        g.round_rect_fill(x, y, w, h, theme::radius_sm(), theme::text_mute());
    }
    if let Some((preview_x, preview_y, preview_w, preview_h)) = layout.preview.filter(|rect| rect.3 > 0.0) {
        let left = preview_x + 10.0;
        let w = (preview_w - 20.0).max(0.0);
        let py = preview_y + 10.0;
        g.round_rect_fill(preview_x, preview_y, preview_w, preview_h, theme::radius_md(), theme::surface());
        g.round_rect_stroke(preview_x, preview_y, preview_w, preview_h, theme::radius_md(), 1.0,
            theme::with_alpha(theme::edge_on(theme::surface()), 90));
        text(
            g,
            left,
            py,
            w - 32.0,
            state.selected.as_deref().unwrap_or(""),
            10.5,
            theme::text_dim(),
            false,
        );
        let rect = native_controls::icon_button(
            g,
            (left + w - 26.0, py - 5.0, 26.0, 26.0),
            cursor,
            "x",
            Default::default(),
        );
        if let Some(rect) = g.clip_hit(rect) {
            state.hits.push((Action::ClosePreview, rect));
        }
        let body_y = py + 25.0;
        let body_h = (preview_y + preview_h - body_y - 10.0).max(0.0);
        g.push_clip(left, body_y, w, body_h);
        match state.preview.as_ref() {
            Some(Ok(Preview::Image {
                rgba,
                width,
                height,
            })) => {
                if !state.preview_uploaded {
                    g.drop_image("kasa-share:preview");
                    g.upload_image("kasa-share:preview", rgba, *width, *height);
                    state.preview_uploaded = true;
                }
                g.queue_image("kasa-share:preview", left, body_y, w, body_h, 1.0, 0.0, 0.0);
            }
            Some(Ok(Preview::Text(body))) => {
                let lines = crate::native_settings::wrap_text(g, body, w, 10.5);
                for (index, (line, _)) in lines.iter().take((body_h / 18.0).ceil() as usize).enumerate()
                {
                    text(
                        g,
                        left,
                        body_y + index as f32 * 18.0,
                        w,
                        line,
                        10.5,
                        theme::text(),
                        false,
                    );
                }
            }
            Some(Ok(Preview::Other)) => text(
                g,
                left,
                body_y,
                w,
                "폴더에서 이 파일을 열 수 있어요",
                12.0,
                theme::text_dim(),
                false,
            ),
            Some(Err(error)) => text(g, left, body_y, w, error, 12.0, theme::text_dim(), false),
            None => text(
                g,
                left,
                body_y,
                w,
                "미리보기를 읽는 중…",
                12.0,
                theme::text_dim(),
                false,
            ),
        }
        g.pop_clip();
    }
    g.rect(left, footer_y, w, 1.0, theme::border());
    let status = &state.catalog.status;
    let failed = status.peers.iter().filter(|peer| !peer.ok).count();
    let summary = if status.paused_deletes > 0 {
        format!("삭제 {}개 보류 · 설정에서 확인", status.paused_deletes)
    } else if failed > 0 {
        format!("기기 {failed}대 연결 확인 필요")
    } else if status.updated_ms == 0 {
        "기기 간 동기화 확인 중…".into()
    } else if kasa_mcp::share::now_ms().saturating_sub(status.updated_ms) > 90_000 {
        "동기화 확인 지연 · 받은 파일 표시 중".into()
    } else if status.peers.is_empty() {
        format!(
            "파일 {}개 · {} · 다른 기기 연결 대기",
            state.catalog.files.len(),
            size(status.bytes)
        )
    } else {
        format!(
            "파일 {}개 · {} · 기기 {}대 받기 확인",
            state.catalog.files.len(),
            size(status.bytes),
            status.peers.len()
        )
    };
    text(
        g,
        left,
        footer_y + 9.0,
        w,
        state.notice.as_deref().unwrap_or(&summary),
        10.5,
        theme::text_dim(),
        false,
    );
    button(
        g,
        state,
        cursor,
        (left, footer_y + 32.0, (w - 8.0) * 0.6, 26.0),
        "폴더에서 보기",
        Action::Reveal,
        false,
    );
    button(
        g,
        state,
        cursor,
        (
            left + (w - 8.0) * 0.6 + 8.0,
            footer_y + 32.0,
            (w - 8.0) * 0.4,
            26.0,
        ),
        "경로 복사",
        Action::CopyPath,
        false,
    );
    g.pop_clip();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nava_shared_preview_reflows_without_covering_files_or_footer() {
        for width in [180.0, 280.0, 390.0, 559.0, 560.0, 900.0] {
            for bottom in [220.0, 400.0, 780.0] {
                for selected in [false, true] {
                    let l = content_layout(14.0, 108.0, width, bottom, selected);
                    assert!(l.list.2 >= 0.0 && l.list.3 >= 0.0);
                    assert!(l.list.0 + l.list.2 <= 14.0 + width);
                    assert!(l.list.1 + l.list.3 <= l.footer);
                    assert_eq!(l.preview.is_some(), selected);
                    if let Some(p) = l.preview {
                        assert!(p.0 + p.2 <= 14.0 + width + 0.01 && p.1 + p.3 <= l.footer + 0.01);
                        let overlap_x = (p.0 + p.2).min(l.list.0 + l.list.2) - p.0.max(l.list.0);
                        let overlap_y = (p.1 + p.3).min(l.list.1 + l.list.3) - p.1.max(l.list.1);
                        assert!(overlap_x <= 0.01 || overlap_y <= 0.01);
                        assert_eq!(p.0 > l.list.0, width >= 560.0);
                    }
                }
            }
        }
    }
}
