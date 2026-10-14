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
    let footer_y = (bottom - 72.0).max(y);
    let preview_height = if state.selected.is_some() {
        ((footer_y - y) * 0.5).clamp(0.0, 240.0)
    } else {
        0.0
    };
    let list_bottom = footer_y - preview_height;
    let list_h = (list_bottom - y).max(0.0);
    state.scroll_max = (state.rows.len() as f32 * 40.0 - list_h).max(0.0);
    state.scroll = state.scroll.clamp(0.0, state.scroll_max);
    state.scroll_track = (state.scroll_max > 0.0).then_some((x + width - 8.0, y, 8.0, list_h));
    g.push_clip(left, y, w, list_h);
    if let Some(problem) = state.problem.as_deref() {
        text(
            g,
            left,
            y + 12.0,
            w,
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
            w,
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
        text(g, left, y + 12.0, w, empty, 12.0, theme::text_dim(), false);
    } else {
        let start = (state.scroll / 40.0).floor() as usize;
        let end = (start + (list_h / 40.0).ceil() as usize + 1).min(state.rows.len());
        for index in start..end {
            let row = &state.rows[index];
            let ry = y + index as f32 * 40.0 - state.scroll;
            let rect = (left, ry, w, 40.0);
            let hovered = contains(rect, cursor);
            let selected = state.selected.as_deref() == Some(&row.path);
            if hovered || selected {
                g.round_rect_fill(
                    left,
                    ry,
                    w,
                    40.0,
                    theme::radius_sm(),
                    theme::surface_hover(),
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
            g.queue_icon(icon, left + 4.0, ry + 11.0, 16.0, theme::text_dim());
            text(
                g,
                left + 28.0,
                ry + 4.0,
                w - 34.0,
                &row.title,
                12.0,
                theme::text(),
                selected,
            );
            text(
                g,
                left + 28.0,
                ry + 23.0,
                w - 34.0,
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
    if preview_height > 0.0 {
        let py = list_bottom + 8.0;
        g.rect(left, list_bottom, w, 1.0, theme::border());
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
        let body_h = (footer_y - body_y - 8.0).max(0.0);
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
                for (index, line) in body
                    .lines()
                    .take((body_h / 18.0).ceil() as usize)
                    .enumerate()
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
