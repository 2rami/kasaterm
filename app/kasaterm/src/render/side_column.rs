use super::*;

/// 칼럼이 이 프레임에 읽는 값. `g` 가 `self.gpu` 를 잡은 안쪽에서 부르므로
/// `&self` 로 구해야 하는 것은 부르는 쪽이 미리 풀어 넘긴다.
pub(super) struct Frame<'a> {
    pub x: f32,
    pub w: f32,
    pub bg: [u8; 4],
    pub viewport: (f32, f32),
    pub status_h: f32,
    pub cursor: (f32, f32),
    pub time_secs: f32,
    pub git_view: &'a GitColView,
    pub repo_list: &'a [std::path::PathBuf],
    pub repo_choice: Option<&'a std::path::PathBuf>,
    pub frozen_info: Option<f32>,
}

pub(super) struct Panels<'a> {
    pub git: &'a mut state::GitState,
    pub info: &'a mut state::InfoState,
    pub sessions: &'a mut state::SessionsColState,
    pub mcp: &'a mut state::McpColState,
    pub closed_panes: &'a [ClosedPane],
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, p: Panels<'_>, f: &Frame<'_>) {
    // 상태줄은 늘 있으므로 조건 없이 함께 뺀다 — 안 빼면 칼럼 바닥(Git 탭은 최근 커밋
    // 목록의 마지막 줄)이 그 띠 뒤로 들어가 가려진다. 시저는 `push_clip` 을 세운 자리에만
    // 걸리는데 여기는 그 바깥이라, 자리를 미리 빼 두는 이 계산이 정본이다.
    let top = TITLE_HEIGHT;
    let bottom = (f.viewport.1 - f.status_h).max(top);
    let Panels { git, info, sessions, mcp, closed_panes } = p;
    match info.tab {
        state::SideTab::Git => {
            git.use_panel_snapshot(f.git_view);
            let y = head(g, info, git, f, top, bottom);
            paint_git(g, git, f, y, top, bottom);
        }
        state::SideTab::Info => {
            let body_top = head(g, info, git, f, top, bottom);
            info::draw_info_col(g, f.cursor, info, f.x, f.w, body_top, bottom);
            // 「—」 의 까닭·잘린 값의 전문 — 표를 다 그린 뒤 커서 아래 것 하나만.
            let (cx, cy) = f.cursor;
            if info.ctx_menu.is_none() {
                if let Some((tip, r)) = info.tip_rects.iter()
                    .find(|(_, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
                {
                    App::draw_hover_tip(g, tip, r.0, r.1 + r.3, f.viewport.0, f.viewport.1);
                }
            }
        }
        state::SideTab::Sessions => {
            let body_top = head(g, info, git, f, top, bottom);
            sesscol::draw_sessions_col(
                g,
                f.cursor,
                sessions,
                closed_panes,
                f.x,
                f.w,
                body_top,
                bottom,
                f.frozen_info,
            );
        }
        state::SideTab::Mcp => {
            let body_top = head(g, info, git, f, top, bottom);
            mcpcol::draw_mcp_col(g, f.cursor, mcp, f.x, f.w, body_top, bottom);
        }
    }
}

fn head(
    g: &mut gpu::GpuRenderer,
    info: &mut state::InfoState,
    git: &mut state::GitState,
    f: &Frame<'_>,
    top: f32,
    bottom: f32,
) -> f32 {
    g.rect(f.x, top, f.w, bottom - top, f.bg);
    g.rect(f.x, top, 1.0, bottom - top, theme::border());
    info::draw_side_tabs(g, f.cursor, info, git, f.x, f.w, top)
}

fn paint_git(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    f: &Frame<'_>,
    mut y: f32,
    top: f32,
    bottom: f32,
) {
    let (git_col_x, git_col_w) = (f.x, f.w);
    let git_view = f.git_view;
    let gcx0 = git_col_x + 14.0;
    let gcw = (git_col_w - 28.0).max(0.0);

    y = git_panel::header(g, git, git_view, f.cursor, gcx0, y, gcw);
    // ── Row 2: ⎇ Uncommitted changes ···· [ ⎯o Commit | ▾ ]
    let list_top;
    let commits_cap = (bottom - TITLE_HEIGHT) * 0.72;
    git.col_commit_want.store(git_panel::requested_history_count(commits_cap), std::sync::atomic::Ordering::Relaxed);
    let commits_h = git_panel::history_height(git_view, &git, commits_cap);
    let input_top = bottom - commits_h;
    if git_view.no_repo || git_view.loading || git_view.issue.is_some() {
        let notice = git_view.issue.as_deref().unwrap_or(if git_view.loading { "Git 정보를 읽는 중이에요" } else { "Git 저장소가 아니에요" });
        let notice = info::fit_text(g, notice, gcw, 12.0, false);
        g.draw_text(
            gcx0,
            y,
            &notice,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        git.commit_btn_rect = None;
        git.commit_caret_rect = None;
        list_top = y + 8.0;
    } else if git.branch_menu_open {
        list_top = y;
    } else if git_view.remote.is_some() {
        g.draw_text(gcx0, y, "원격 Git · 읽기 전용", gpu::DrawOpts {
            font_size: 10.5, color: theme::text_dim(), bold: false, italic: false,
        });
        list_top = y + native_controls::CONTROL_HEIGHT;
    } else {
        let bh = 24.0_f32;
        let by = y - 4.0;
        let caret_w = 20.0_f32;
        let busy = git.op;
        let can_commit = !git_view.staged.is_empty() || !git_view.unstaged.is_empty();
        // While a git op runs, the button shows a spinner + "Pushing…"
        // and ignores clicks. No uncommitted changes but commits to
        // sync → the primary button becomes the sync action
        // (GitHub-Desktop style); with changes it's Commit. 당길 것이
        // 있으면 **Pull 이 Push 보다 먼저**다(2026-08-16 「풀있을때 풀먼저
        // 뜨게」) — 당기기 전의 Push 는 어차피 원격이 거절한다. The caret
        // dropdown always offers the full set (Commit / Push / Pull /
        // Create PR).
        let pull_mode = busy.is_none() && !can_commit && git_view.behind > 0;
        let push_mode =
            busy.is_none() && !can_commit && !pull_mode && git_view.ahead > 0;
        let can_drop =
            busy.is_none() && (can_commit || git_view.ahead > 0 || git_view.behind > 0);
        let main_active = busy.is_none() && (can_commit || push_mode || pull_mode);
        let main_label = if let Some(op) = busy {
            format!("{op}…")
        } else if pull_mode {
            format!("Pull  {}", git_view.behind)
        } else if push_mode {
            format!("Push  {}", git_view.ahead)
        } else {
            "Commit".to_string()
        };
        let main_icon = if pull_mode {
            "arrow-down"
        } else if push_mode {
            "arrow-up"
        } else {
            "git-commit-horizontal"
        };
        let lw = g.measure_chrome_text(&main_label, 12.0, true);
        let main_w = 24.0 + lw + 10.0;
        let total_w = main_w + caret_w;
        let bx = git_col_x + git_col_w - 12.0 - total_w;
        // 라벨은 버튼이 자리를 잡은 **뒤** 남는 폭에 맞춘다. 먼저 그리면
        // 좁은 칼럼에서 그대로 버튼 밑으로 파고든다(216px 실측: 라벨은
        // 162 까지 뻗는데 버튼이 105 에서 시작했다). 잘라서 「Uncomm…」 을
        // 남기는 대신 짧은 말로 갈아탄다 — 말줄임한 영어는 못 읽는다.
        {
            let room = (bx - 10.0 - (gcx0 + 18.0)).max(0.0);
            let lbl =
                if g.measure_chrome_text("Uncommitted changes", 12.0, true) <= room {
                    "Uncommitted changes"
                } else if g.measure_chrome_text("Changes", 12.0, true) <= room {
                    "Changes"
                } else {
                    ""
                };
            if !lbl.is_empty() {
                g.queue_icon("git-branch", gcx0, y + 1.0, 13.0, theme::text_mute());
                g.draw_text(
                    gcx0 + 18.0,
                    y,
                    lbl,
                    gpu::DrawOpts {
                        font_size: 12.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
            }
        }
        let mhov = f.cursor.0 >= bx
            && f.cursor.0 <= bx + main_w
            && f.cursor.1 >= by
            && f.cursor.1 <= by + bh;
        let chov = f.cursor.0 >= bx + main_w
            && f.cursor.0 <= bx + total_w
            && f.cursor.1 >= by
            && f.cursor.1 <= by + bh;
        let base = if can_drop || busy.is_some() {
            theme::surface_active()
        } else {
            theme::with_alpha(theme::surface_hover(), 0x66)
        };
        round_rect(g, bx, by, total_w, bh, theme::radius_sm(), base);
        if main_active && mhov {
            round_rect(g, bx, by, main_w, bh, theme::radius_sm(), theme::accent());
        }
        if can_drop && chov {
            round_rect(
                g,
                bx + main_w,
                by,
                caret_w,
                bh,
                theme::radius_sm(),
                theme::accent(),
            );
        }
        g.rect(
            bx + main_w,
            by + 5.0,
            1.0,
            bh - 10.0,
            theme::with_alpha(theme::bg(), 0x99),
        );
        let fg_main = if main_active || busy.is_some() {
            theme::text()
        } else {
            theme::text_mute()
        };
        let fg_caret = if can_drop {
            theme::text()
        } else {
            theme::text_mute()
        };
        if busy.is_some() {
            // Spinner: 8 dots round the icon slot, the bright one
            // chasing round once a second.
            let scx = bx + 14.0;
            let scy = by + bh / 2.0;
            let head = (f.time_secs * 1.1).fract();
            for i in 0..8 {
                let ang = (i as f32 / 8.0) * std::f32::consts::TAU
                    - std::f32::consts::FRAC_PI_2;
                let p = i as f32 / 8.0;
                let mut dd = head - p;
                if dd < 0.0 {
                    dd += 1.0;
                }
                let a = (1.0 - dd).powf(1.6);
                let d = 1.5_f32;
                circle_rect(
                    g,
                    scx + ang.cos() * 5.5 - d,
                    scy + ang.sin() * 5.5 - d,
                    d * 2.0,
                    theme::with_alpha(theme::text(), 30 + (a * 220.0) as u8),
                );
            }
        } else {
            g.queue_icon(main_icon, bx + 8.0, by + (bh - 13.0) / 2.0, 13.0, fg_main);
        }
        g.draw_text(
            bx + 24.0,
            by + (bh - 12.0) / 2.0,
            &main_label,
            gpu::DrawOpts {
                font_size: 12.0,
                color: fg_main,
                bold: true,
                italic: false,
            },
        );
        g.draw_text(
            bx + main_w + (caret_w - 7.0) / 2.0,
            by + (bh - 11.0) / 2.0,
            "▾",
            gpu::DrawOpts {
                font_size: 11.0,
                color: fg_caret,
                bold: false,
                italic: false,
            },
        );
        git.commit_btn_rect = Some((bx, by, main_w, bh));
        git.commit_caret_rect = Some((bx + main_w, by, caret_w, bh));
        y += 24.0;
        g.rect(gcx0, y, gcw, 1.0, theme::with_alpha(theme::border(), 0x80));
        list_top = y + 10.0;
    }
    // 휠이 읽을 기하. 목록이 없는 갈래에서도 반드시 써야 한다 —
    // 안 쓰면 직전 프레임의 값이 남아, 변경이 사라진 뒤에도 휠이
    // 없는 목록을 스크롤한다.
    git.col_list_extent = ((input_top - list_top).max(0.0), 0.0);
    if git_view.clean && !git.branch_menu_open {
        circle_rect(g, gcx0, list_top + 4.0, 8.0, theme::success());
        g.draw_text(
            gcx0 + 15.0,
            list_top + 1.0,
            "변경 없음",
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
    } else {
        let item_h = 22.0_f32;
        let header_h = 21.0_f32;
        let dline_h = 15.0_f32;
        let gutter_w = 30.0_f32;
        let mut rects: Vec<(bool, String, (f32, f32, f32, f32))> = Vec::new();
        let mut stage_rects: Vec<(bool, String, (f32, f32, f32, f32))> = Vec::new();
        let mut discard_rects: Vec<(String, bool, (f32, f32, f32, f32))> = Vec::new();
        let mut open_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        // Two stacked sections (VSCode model). `staged` true =
        // "Staged Changes" (− unstages); false = "Changes" (+
        // stages). Both scroll together off git_col_scroll.
        let mut y_cur = list_top - git.col_scroll;
        // While a menu is up, skip the change list entirely — its
        // text/icons draw in the glyph layer (above the dim quad)
        // so they'd otherwise bleed through the menu.
        let menus_open = git.commit_menu_open
            || git.path_menu_open
            || git.branch_menu_open;
        // 목록은 `list_top`~`input_top` 안에 가둔다. 지금까지는 행마다
        // 「완전히 밖이면 건너뛴다」로만 걸러, 위로 반쯤 걸친 행이 통째로
        // 그려져 Commit 버튼 줄과 구분선을 덮었다. 루프 **밖**에서 한 번만
        // 세운다 — 안에서 세우면 행마다 세그먼트가 둘씩 쌓인다.
        g.push_clip(
            git_col_x,
            list_top,
            git_col_w,
            (input_top - list_top).max(0.0),
        );
        // 커서가 잘려 안 보이는 쪽에 있는데 행의 보이는 쪽에 하이라이트가
        // 그려지는 것은 시저가 못 막는다(그 하이라이트는 클립 안이니까).
        // 그래서 이 목록 안에서는 걸러 낸 커서를 쓴다. 저장되는 히트렉트는
        // 그것과 별개로 루프 끝에서 교집합을 낸다 — `handler.rs` 가 **다음
        // 클릭 좌표로 다시** 검사하므로 커서를 거른 것만으로는 안 된다.
        let cur = match g.clip_hit((f.cursor.0, f.cursor.1, 1.0, 1.0)) {
            Some(_) => f.cursor,
            None => (f32::MIN, f32::MIN),
        };
        for (title, staged, files) in [
            ("Staged Changes", true, &git_view.staged),
            ("Changes", false, &git_view.unstaged),
        ] {
            if files.is_empty() {
                continue;
            }
            // Section header (count) — 완전히 밖일 때만 건너뛴다.
            // 경계는 손으로 다시 쓰지 않고 클립에게 묻는다.
            if !menus_open && g.clip_visible(git_col_x, y_cur, git_col_w, header_h) {
                g.draw_text(
                    gcx0,
                    y_cur + 5.0,
                    &format!("{}  {}", title, files.len()),
                    gpu::DrawOpts {
                        font_size: 11.0,
                        color: theme::text_mute(),
                        bold: true,
                        italic: false,
                    },
                );
            }
            y_cur += header_h;
            for (marker, path) in files.iter() {
                let ry = y_cur;
                y_cur += item_h;
                let expanded = git.col_expanded.contains(&(staged, path.clone()));
                let row_visible =
                    !menus_open && g.clip_visible(git_col_x, ry, git_col_w, item_h);
                if row_visible {
                    let hovered = cur.0 >= git_col_x
                        && cur.0 <= git_col_x + git_col_w
                        && cur.1 >= ry
                        && cur.1 < ry + item_h;
                    if hovered {
                        hover_rect(
                            g,
                            gcx0 - 5.0,
                            ry,
                            gcw + 10.0,
                            item_h,
                            theme::radius_sm(),
                        );
                    }
                    // Expander chevron at the row's left edge.
                    g.queue_icon(
                        if expanded {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        gcx0,
                        ry + (item_h - 12.0) / 2.0,
                        12.0,
                        theme::text_mute(),
                    );
                    let untracked = *marker == 'U';
                    // Filename bright, parent dir dim after it (so the
                    // name stays readable even when the path is long).
                    // No status badge — chevron + name, cursor-style.
                    let fname = path.rsplit('/').next().unwrap_or(path.as_str());
                    let dir =
                        path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
                    let tx = gcx0 + 20.0;
                    let ty = ry + (item_h - 12.0) / 2.0;
                    // 이름·경로는 여기서 그리지 않는다. 오른쪽 액션과
                    // numstat 이 자리를 잡은 뒤에야 남는 폭을 알 수 있고,
                    // 모르면 216px 칼럼에서 셋이 서로 위에 겹쳐 그려진다.
                    // Action cluster (cursor style), always visible
                    // right-to-left: +/− stage · ↩ discard · ⤴ open.
                    // numstat (+ins -del) sits just left of them.
                    let aw = 19.0_f32;
                    let agap = 1.0_f32;
                    // 좁은 칼럼에서 액션 셋(59px)과 numstat 이 늘 자리를
                    // 지키면 정작 파일 이름에 55px 밖에 안 남아 「info.…」
                    // 로 뭉개진다(216px 실측). 이 폭에서는 이름이 내용이고
                    // 버튼은 손이 갈 때만 필요하니, 가리킨 행에서만 꺼낸다.
                    let narrow = git_col_w < GIT_DENSE_COMPACT;
                    let show_acts = git_view.remote.is_none() && (!narrow || hovered);
                    let mut ax =
                        git_col_x + git_col_w - 12.0 - if show_acts { aw } else { 0.0 };
                    let icon_dim = if hovered {
                        theme::text_dim()
                    } else {
                        theme::with_alpha(theme::text_dim(), 0x88)
                    };
                    if show_acts {
                        let bh = cur.0 >= ax
                            && cur.0 <= ax + aw
                            && cur.1 >= ry
                            && cur.1 < ry + item_h;
                        if bh {
                            round_rect(
                                g,
                                ax,
                                ry + 2.0,
                                aw,
                                18.0,
                                theme::radius_sm(),
                                theme::surface_active(),
                            );
                        }
                        g.queue_icon(
                            if staged { "minus" } else { "plus" },
                            ax + (aw - 13.0) / 2.0,
                            ry + (item_h - 13.0) / 2.0,
                            13.0,
                            if bh { theme::text() } else { icon_dim },
                        );
                        stage_rects.push((
                            !staged,
                            path.clone(),
                            (ax - 1.0, ry, aw + 2.0, item_h),
                        ));
                        ax -= aw + agap;
                    }
                    if show_acts {
                        let bh = cur.0 >= ax
                            && cur.0 <= ax + aw
                            && cur.1 >= ry
                            && cur.1 < ry + item_h;
                        if bh {
                            round_rect(
                                g,
                                ax,
                                ry + 2.0,
                                aw,
                                18.0,
                                theme::radius_sm(),
                                theme::surface_active(),
                            );
                        }
                        g.queue_icon(
                            "undo-2",
                            ax + (aw - 13.0) / 2.0,
                            ry + (item_h - 13.0) / 2.0,
                            13.0,
                            if bh { DIFF_RED } else { icon_dim },
                        );
                        discard_rects.push((
                            path.clone(),
                            untracked,
                            (ax - 1.0, ry, aw + 2.0, item_h),
                        ));
                        ax -= aw + agap;
                    }
                    if show_acts {
                        let bh = cur.0 >= ax
                            && cur.0 <= ax + aw
                            && cur.1 >= ry
                            && cur.1 < ry + item_h;
                        if bh {
                            round_rect(
                                g,
                                ax,
                                ry + 2.0,
                                aw,
                                18.0,
                                theme::radius_sm(),
                                theme::surface_active(),
                            );
                        }
                        g.queue_icon(
                            "external-link",
                            ax + (aw - 13.0) / 2.0,
                            ry + (item_h - 13.0) / 2.0,
                            13.0,
                            if bh { theme::text() } else { icon_dim },
                        );
                        open_rects
                            .push((path.clone(), (ax - 1.0, ry, aw + 2.0, item_h)));
                    }
                    // numstat — right-aligned just left of the actions.
                    let mut text_right = ax - 4.0;
                    if let Some((ins, del)) = git_view
                        .numstat
                        .get(path)
                        .filter(|_| !(narrow && show_acts))
                    {
                        if *ins > 0 || *del > 0 {
                            let minus = format!("-{del}");
                            let plus = format!("+{ins}");
                            let wm = g.measure_chrome_text(&minus, 11.0, false);
                            let wp = g.measure_chrome_text(&plus, 11.0, false);
                            let mut rx = ax - 4.0;
                            if *del > 0 {
                                rx -= wm;
                                g.draw_text(
                                    rx,
                                    ty,
                                    &minus,
                                    gpu::DrawOpts {
                                        font_size: 11.0,
                                        color: DIFF_RED,
                                        bold: false,
                                        italic: false,
                                    },
                                );
                                rx -= 5.0;
                            }
                            if *ins > 0 {
                                rx -= wp;
                                g.draw_text(
                                    rx,
                                    ty,
                                    &plus,
                                    gpu::DrawOpts {
                                        font_size: 11.0,
                                        color: theme::success(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                            text_right = rx;
                        }
                    }
                    // 이제 남은 폭이 확정됐다. 이름을 먼저 채우고,
                    // 부모 경로는 그러고도 남을 때만 — 좁은 칼럼에서
                    // 알아야 할 쪽은 경로가 아니라 파일 이름이다.
                    {
                        let avail = (text_right - 6.0 - tx).max(0.0);
                        let nm = crate::info::fit_text(g, fname, avail, 12.0, false);
                        let endx = if nm.is_empty() {
                            tx
                        } else {
                            g.draw_text(
                                tx,
                                ty,
                                &nm,
                                gpu::DrawOpts {
                                    font_size: 12.0,
                                    color: theme::text(),
                                    bold: false,
                                    italic: false,
                                },
                            )
                        };
                        if !dir.is_empty() && !nm.is_empty() && nm == fname {
                            let room = (text_right - 6.0 - (endx + 7.0)).max(0.0);
                            let d =
                                crate::info::fit_text_tail(g, dir, room, 11.0, false);
                            if !d.is_empty() {
                                g.draw_text(
                                    endx + 7.0,
                                    ty + 0.5,
                                    &d,
                                    gpu::DrawOpts {
                                        font_size: 11.0,
                                        color: theme::text_mute(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                        }
                    }
                    rects.push((
                        staged,
                        path.clone(),
                        (git_col_x, ry, git_col_w, item_h),
                    ));
                }
                // Inline unified diff for an expanded row, syntax-
                // highlighted with the same tokenizer the code-block
                // overlay uses. Numbered gutter + tinted +/- bands.
                if expanded {
                    let lang = code_lang_for_path(std::path::Path::new(path.as_str()));
                    if let Some(rows_d) =
                        git.col_diff_cache.get(&(staged, path.clone()))
                    {
                        for dl in rows_d.iter() {
                            let dy = y_cur;
                            y_cur += dline_h;
                            if !g.clip_visible(git_col_x, dy, git_col_w, dline_h) {
                                continue;
                            }
                            use kasa_mcp::git::DiffLineKind as K;
                            let (bg, sign, scol) = match dl.kind {
                                K::Add => (
                                    theme::with_alpha(theme::success(), 0x22),
                                    "+",
                                    theme::success(),
                                ),
                                K::Del => {
                                    (theme::with_alpha(DIFF_RED, 0x22), "-", DIFF_RED)
                                }
                                K::Hunk => (
                                    theme::with_alpha(theme::accent(), 0x14),
                                    "",
                                    theme::text_mute(),
                                ),
                                K::Context => ([0, 0, 0, 0], " ", theme::text_mute()),
                            };
                            if bg[3] > 0 {
                                g.rect(gcx0 - 5.0, dy, gcw + 10.0, dline_h, bg);
                            }
                            if dl.kind == K::Hunk {
                                g.draw_text(
                                    gcx0,
                                    dy + 1.5,
                                    dl.text.trim_end(),
                                    gpu::DrawOpts {
                                        font_size: 10.0,
                                        color: theme::text_mute(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                                continue;
                            }
                            // Line number gutter (new side, else old).
                            if let Some(n) = dl.new_no.or(dl.old_no) {
                                let ns = n.to_string();
                                let nw = g.measure_chrome_text(&ns, 10.0, false);
                                g.draw_text(
                                    gcx0 + gutter_w - nw - 4.0,
                                    dy + 1.5,
                                    &ns,
                                    gpu::DrawOpts {
                                        font_size: 10.0,
                                        color: theme::text_mute(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                            g.draw_text(
                                gcx0 + gutter_w,
                                dy + 1.5,
                                sign,
                                gpu::DrawOpts {
                                    font_size: 11.0,
                                    color: scol,
                                    bold: false,
                                    italic: false,
                                },
                            );
                            let mut tx = gcx0 + gutter_w + 9.0;
                            for (tok, col) in gpu::highlight_code_line(
                                dl.text.trim_end(),
                                lang,
                                theme::text_dim(),
                            ) {
                                tx = g.draw_text(
                                    tx,
                                    dy + 1.5,
                                    &tok,
                                    gpu::DrawOpts {
                                        font_size: 11.0,
                                        color: col,
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        // 히트렉트를 목록 구역과 교집합 낸다. 이 넷은 `handler.rs` 가
        // **다음 클릭 좌표로 다시** 검사하므로, 위에서 커서를 거른 것과는
        // 별개로 rect 자체가 잘려 있어야 한다. 안 자르면 화면은 완벽한데
        // Commit 버튼 뒤로 스크롤된 행의 「되돌리기」가 눌린다 — 되돌릴 수
        // 없는 동작이고, 스크린샷이 절대 못 잡는 부류다.
        macro_rules! clip_rects {
            ($v:expr, $i:tt) => {
                $v.retain_mut(|e| match g.clip_hit(e.$i) {
                    Some(h) => {
                        e.$i = h;
                        true
                    }
                    None => false,
                })
            };
        }
        clip_rects!(rects, 2);
        clip_rects!(stage_rects, 2);
        clip_rects!(discard_rects, 2);
        clip_rects!(open_rects, 1);
        g.pop_clip();
        // 휠에게 넘기는 기하. `y_cur` 는 `list_top - col_scroll` 에서
        // 출발해 섹션 머리·파일 행·펼친 diff 줄을 **실제로 그린 만큼**
        // 지나왔으므로, 스크롤을 되더하면 그게 곧 내용 높이다. 휠이
        // 자기 힘으로는 못 구하는 값이라(펼친 diff 는 캐시를 뒤져야
        // 나온다) 여기서 써 준다.
        git.col_list_extent = (
            (input_top - list_top).max(0.0),
            (y_cur + git.col_scroll - list_top).max(0.0),
        );
        git.col_file_rects = rects;
        git.col_stage_rects = stage_rects;
        git.col_discard_rects = discard_rects;
        git.col_open_rects = open_rects;
    }
    git_panel::history(g, git, git_view, f.cursor, gcx0, input_top, gcw, bottom - 2.0);
    // Dropdowns (path picker / branch switcher) paint last so they
    // overlay the list + buttons. Built from the precomputed repo
    // list and the poller's branch list.
    git_paint_dropdowns(
        g,
        git_col_x,
        git_col_w,
        TITLE_HEIGHT,
        git.path_hdr_rect,
        git.path_menu_open,
        f.repo_list,
        &git.col_pinned_cwd,
        &mut git.path_menu_rects,
        if git_view.remote.is_none() { &git_view.repos } else { &[] },
        git_view.repos.iter().find(|r| Some(*r) == f.repo_choice).map(|r| r.as_path()),
        &mut git.path_menu_repo_rects,
    );
    git_panel::branches(g, git, git_view, f.cursor, gcx0, gcw, bottom);
    // ── Commit-button dropdown (Commit / Push / Create PR)
    git.commit_menu_rects.clear();
    if git.commit_menu_open {
        if let Some((ccx, ccy, ccw, cch)) = git.commit_caret_rect {
            // Dim the panel behind the menu so the change-list rows
            // (and their hover buttons) don't bleed alongside it.
            g.rect(
                git_col_x,
                top,
                git_col_w,
                bottom - top,
                theme::with_alpha([0, 0, 0, 255], 0xB0),
            );
            // Push/Pull carry their ahead/behind counts so you can
            // see what's pending before clicking.
            let push_label = if git_view.ahead > 0 {
                format!("Push  {}", git_view.ahead)
            } else {
                "Push".to_string()
            };
            let pull_label = if git_view.behind > 0 {
                format!("Pull  {}", git_view.behind)
            } else {
                "Pull".to_string()
            };
            // 당길 것이 있으면 Pull 을 Push 위로 — 기본 버튼과 같은 우선
            // 순위(당기기 전의 Push 는 원격이 거절한다).
            let (sync_a, sync_b) = if git_view.behind > 0 {
                (
                    ("arrow-down", pull_label, GitCommitAction::Pull),
                    ("arrow-up", push_label, GitCommitAction::Push),
                )
            } else {
                (
                    ("arrow-up", push_label, GitCommitAction::Push),
                    ("arrow-down", pull_label, GitCommitAction::Pull),
                )
            };
            let items: [(&str, String, GitCommitAction); 4] = [
                (
                    "git-commit-horizontal",
                    "Commit".to_string(),
                    GitCommitAction::Commit,
                ),
                sync_a,
                sync_b,
                ("github", "Create PR".to_string(), GitCommitAction::CreatePr),
            ];
            let iw = 190.0_f32;
            let ih = 34.0_f32;
            let mh = ih * items.len() as f32 + 8.0;
            let mx = (ccx + ccw - iw).max(git_col_x + 8.0);
            let my = ccy + cch + 4.0;
            panel_rect_outlined(
                g,
                mx,
                my,
                iw,
                mh,
                theme::radius_md(),
                theme::surface(),
            );
            let mut iy = my + 4.0;
            for (icon, label, act) in items {
                let hov = f.cursor.0 >= mx
                    && f.cursor.0 <= mx + iw
                    && f.cursor.1 >= iy
                    && f.cursor.1 <= iy + ih;
                if hov {
                    hover_rect(g, mx + 4.0, iy, iw - 8.0, ih, theme::radius_sm());
                }
                g.queue_icon(
                    icon,
                    mx + 14.0,
                    iy + (ih - 15.0) / 2.0,
                    15.0,
                    theme::text_dim(),
                );
                g.draw_text(
                    mx + 38.0,
                    iy + (ih - 13.0) / 2.0,
                    &label,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
                git.commit_menu_rects.push((act, (mx, iy, iw, ih)));
                iy += ih;
            }
        }
    }
}
