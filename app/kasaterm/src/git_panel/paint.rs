use super::*;
use crate::screenread::DIFF_RED;

pub(crate) struct Frame<'a> {
    pub cursor: (f32, f32),
    pub time_secs: f32,
    pub git_view: &'a GitColView,
    pub repo_list: &'a [std::path::PathBuf],
    pub repo_choice: Option<&'a std::path::PathBuf>,
}

pub(crate) fn panel(
    g: &mut gpu::GpuRenderer,
    git: &mut state::GitState,
    f: &Frame<'_>,
    bounds: (f32, f32, f32, f32),
) {
    let (git_col_x, top, git_col_w, height) = bounds;
    let bottom = top + height;
    let mut y = top;
    let git_view = f.git_view;
    let local = git_view.remote.is_none();
    let Some(visible) = g.clip_hit(bounds) else { return };
    let visible_bottom = visible.1 + visible.3;
    let gcx0 = git_col_x + 14.0;
    let gcw = (git_col_w - 28.0).max(0.0);

    y = git_panel::header(g, git, git_view, f.cursor, gcx0, y, gcw);
    // ── Row 2: ⎇ Uncommitted changes ···· [ ⎯o Commit | ▾ ]
    let list_top;
    let commits_cap = (bottom - y - 104.0).max(GIT_COMMITS_H_MIN);
    git.col_commit_want.store(git_panel::requested_history_count(commits_cap), std::sync::atomic::Ordering::Relaxed);
    let commits_h = git_panel::history_height(git_view, git, commits_cap);
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
        git.commit_btn_rect = main_active.then_some((bx, by, main_w, bh));
        git.commit_caret_rect = can_drop.then_some((bx + main_w, by, caret_w, bh));
        y += 24.0;
        g.rect(gcx0, y, gcw, 1.0, theme::with_alpha(theme::border(), 0x80));
        list_top = y + 10.0;
    }
    let content = [(true, &git_view.staged), (false, &git_view.unstaged)]
        .into_iter()
        .filter(|(_, files)| !files.is_empty())
        .map(|(staged, files)| {
            21.0 + files.iter().map(|(_, path)| {
                let key = (staged, path.clone());
                22.0 + if git.col_expanded.contains(&key) {
                    git.col_diff_cache.get(&key).map_or(0.0, |rows| rows.len() as f32 * 15.0)
                } else { 0.0 }
            }).sum::<f32>()
        }).sum::<f32>();
    let input_top = (bottom - commits_h).min(list_top + content.max(24.0) + 8.0);
    let list_height = (input_top - list_top).max(0.0);
    git.col_scroll = git.col_scroll.clamp(0.0, (content - list_height).max(0.0));
    // 휠이 읽을 기하. 목록이 없는 갈래에서도 반드시 써야 한다 —
    // 안 쓰면 직전 프레임의 값이 남아, 변경이 사라진 뒤에도 휠이
    // 없는 목록을 스크롤한다.
    git.col_list_extent = ((input_top - list_top).max(0.0), 0.0);
    git.list_rect = g.clip_hit((git_col_x, list_top, git_col_w, git.col_list_extent.0));
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
                    let hovered = local && cur.0 >= git_col_x
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
                    if local {
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
                    }
                    let untracked = *marker == 'U';
                    // Filename bright, parent dir dim after it (so the
                    // name stays readable even when the path is long).
                    // No status badge — chevron + name, cursor-style.
                    let fname = path.rsplit('/').next().unwrap_or(path.as_str());
                    let dir =
                        path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
                    let tx = gcx0 + if local { 20.0 } else { 0.0 };
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
                    let show_acts = local && (!narrow || hovered);
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
                    if local {
                        rects.push((staged, path.clone(), (git_col_x, ry, git_col_w, item_h)));
                    }
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
                            use kasa_git::git::DiffLineKind as K;
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
    git_panel::history(g, git, git_view, f.cursor, gcx0, input_top, gcw, input_top + commits_h - 2.0);
    // Dropdowns (path picker / branch switcher) paint last so they
    // overlay the list + buttons. Built from the precomputed repo
    // list and the poller's branch list.
    git_paint_dropdowns(
        g,
        git_col_x,
        git_col_w,
        (visible.1, visible_bottom),
        git.path_hdr_rect,
        git.path_menu_open,
        f.repo_list,
        git.col_pinned_cwd.as_deref(),
        &mut git.path_menu_rects,
        if git_view.remote.is_none() { &git_view.repos } else { &[] },
        git_view.repos.iter().find(|r| Some(*r) == f.repo_choice).map(|r| r.as_path()),
        &mut git.path_menu_repo_rects,
    );
    git_panel::branches(g, git, git_view, f.cursor, gcx0, gcw, visible_bottom);
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
            let my = (ccy + cch + 4.0).min(visible_bottom - mh).max(visible.1);
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
                if let Some(rect) = g.clip_hit((mx, iy, iw, ih)) {
                    git.commit_menu_rects.push((act, rect));
                }
                iy += ih;
            }
        }
    }
    git.path_hdr_rect = git.path_hdr_rect.and_then(|r| g.clip_hit(r));
    git.branch_hdr_rect = git.branch_hdr_rect.and_then(|r| g.clip_hit(r));
    git.commit_btn_rect = git.commit_btn_rect.and_then(|r| g.clip_hit(r));
    git.commit_caret_rect = git.commit_caret_rect.and_then(|r| g.clip_hit(r));
}

/// 하위 레포 고르기 메뉴에 싣는 줄 수 — 그 아래는 덜 최근이라 고를 일이 드물다.
const GIT_NESTED_MENU_ROWS: usize = 8;

/// Paint the git-column header dropdowns (repo path picker + branch switcher)
/// and fill their click rects. A free fn, not a method, so it can run inside
/// the `&mut self.gpu` block (which can't re-borrow `&self`): the caller hands
/// it `g` plus the disjoint rect Vecs. Drawn last so it overlays the list.
#[allow(clippy::too_many_arguments)]
fn git_paint_dropdowns(
    g: &mut gpu::GpuRenderer,
    col_x: f32,
    col_w: f32,
    (top, bottom): (f32, f32),
    path_hdr: Option<(f32, f32, f32, f32)>,
    path_open: bool,
    repos: &[std::path::PathBuf],
    pinned: Option<&std::path::Path>,
    path_rects: &mut Vec<(Option<std::path::PathBuf>, (f32, f32, f32, f32))>,
    nested: &[std::path::PathBuf],
    chosen: Option<&std::path::Path>,
    nested_rects: &mut Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
) {
    let item_h = 28.0_f32;
    let pad = 6.0_f32;
    let px = col_x + 6.0;
    let pw = (col_w - 12.0).max(0.0);
    // A raised menu panel with a 1px border so it reads above the list.
    let panel = |g: &mut gpu::GpuRenderer, y: f32, h: f32| {
        round_rect(
            g,
            px - 1.0,
            y - 1.0,
            pw + 2.0,
            h + 2.0,
            theme::radius_md(),
            theme::border(),
        );
        round_rect(g, px, y, pw, h, theme::radius_md(), theme::surface_active());
    };
    let row = |g: &mut gpu::GpuRenderer, iy: f32, label: &str, on: bool| {
        if on {
            round_rect(
                g,
                px + 4.0,
                iy + 1.0,
                pw - 8.0,
                item_h - 2.0,
                theme::radius_sm(),
                theme::with_alpha(theme::accent(), 0x40),
            );
        }
        let col = if on { theme::text() } else { theme::text_dim() };
        let label = info::fit_text(g, label, (pw - 24.0).max(0.0), 12.0, false);
        g.draw_text(
            px + 12.0,
            iy + (item_h - 12.0) / 2.0,
            &label,
            gpu::DrawOpts {
                font_size: 12.0,
                color: col,
                bold: false,
                italic: false,
            },
        );
    };
    if path_open {
        if let Some((_hx, hy, _hw, hh)) = path_hdr {
            // 부모 폴더 칸의 하위 레포 — 최근에 만진 순, 맨 위가 자동으로 보이는 것.
            let slots = ((bottom - top - 2.0 * pad) / item_h).floor().max(1.0) as usize;
            let nested = &nested[..nested.len().min(GIT_NESTED_MENU_ROWS).min(slots - 1)];
            let repos = &repos[..repos.len().min(slots - 1 - nested.len())];
            let n = repos.len() + nested.len() + 1;
            let menu_h = n as f32 * item_h + pad * 2.0;
            let my = (hy + hh + 2.0).min(bottom - menu_h).max(top);
            panel(g, my, menu_h);
            let mut iy = my + pad;
            // "자동 추적" — selected when nothing is pinned or chosen.
            row(g, iy, "자동 추적 (활성 pane)", pinned.is_none() && chosen.is_none());
            if let Some(rect) = g.clip_hit((px, iy, pw, item_h)) { path_rects.push((None, rect)); }
            iy += item_h;
            for (i, r) in nested.iter().enumerate() {
                let name = r.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                let label = if i == 0 { format!("› {name}  ·  최근") } else { format!("› {name}") };
                row(g, iy, &label, pinned.is_none() && chosen == Some(r.as_path()));
                if let Some(rect) = g.clip_hit((px, iy, pw, item_h)) { nested_rects.push((r.clone(), rect)); }
                iy += item_h;
            }
            for r in repos {
                let name = r.file_name().and_then(|s| s.to_str()).unwrap_or("?");
                let sel = pinned == Some(r.as_path());
                row(g, iy, name, sel);
                if let Some(rect) = g.clip_hit((px, iy, pw, item_h)) { path_rects.push((Some(r.clone()), rect)); }
                iy += item_h;
            }
        }
    }
}

pub(crate) fn modal(
    g: &mut gpu::GpuRenderer, git: &mut state::GitState, git_view: &GitColView,
    (win_w, win_h): (f32, f32), cursor: (f32, f32), preedit: &str, commit_caret_on: bool,
) {
    git.commit_modal_rects.clear();
    if git.commit_modal_open {
        // Full-window dim + centered card (not clipped to the git
        // column) so the modal reads as a real dialog and nothing
        // behind it bleeds through.
        g.rect(
            0.0,
            0.0,
            win_w,
            win_h,
            theme::with_alpha([0, 0, 0, 255], 0xCC),
        );
        let bw = 560.0_f32.min((win_w - 32.0).max(0.0));
        let bx = (win_w - bw) / 2.0;
        let bh = (win_h - TITLE_HEIGHT - 24.0).clamp(0.0, 470.0);
        let bxy = TITLE_HEIGHT + (win_h - TITLE_HEIGHT - bh) / 2.0;
        round_rect(
            g,
            bx - 1.0,
            bxy - 1.0,
            bw + 2.0,
            bh + 2.0,
            theme::radius_md(),
            theme::with_alpha(theme::border(), 0xFF),
        );
        round_rect(g, bx, bxy, bw, bh, theme::radius_md(), theme::bg());
        let pad = 18.0_f32;
        let cx = bx + pad;
        let cw = bw - pad * 2.0;
        g.push_clip(bx, bxy, bw, bh);
        let mut my = bxy + pad;
        // Header: icon chip + X
        round_rect(
            g,
            cx,
            my,
            36.0,
            36.0,
            theme::radius_sm(),
            theme::surface_active(),
        );
        g.queue_icon(
            "git-commit-horizontal",
            cx + 10.0,
            my + 10.0,
            16.0,
            theme::text(),
        );
        let xx = bx + bw - pad - 16.0;
        let xhov = cursor.0 >= xx - 5.0
            && cursor.0 <= xx + 21.0
            && cursor.1 >= my
            && cursor.1 <= my + 24.0;
        g.queue_icon(
            "x",
            xx,
            my + 4.0,
            16.0,
            if xhov {
                theme::text()
            } else {
                theme::text_mute()
            },
        );
        git
            .commit_modal_rects
            .push((GitModalBtn::Close, (xx - 5.0, my, 26.0, 26.0)));
        my += 44.0;
        g.draw_text(
            cx,
            my,
            "Commit your changes",
            gpu::DrawOpts {
                font_size: 19.0,
                color: theme::text(),
                bold: true,
                italic: false,
            },
        );
        my += 27.0;
        // Branch
        g.draw_text(
            cx,
            my,
            "Branch",
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        my += 18.0;
        g.queue_icon("git-branch", cx, my, 15.0, theme::text_dim());
        let mbranch = if git_view.branch.is_empty() {
            "—"
        } else {
            git_view.branch.as_str()
        };
        g.draw_text(
            cx + 22.0,
            my + 1.0,
            mbranch,
            gpu::DrawOpts {
                font_size: 14.0,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        my += 28.0;
        // Changes + Include unstaged toggle
        g.draw_text(
            cx,
            my,
            "Changes",
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        let tw = 38.0_f32;
        let th = 20.0_f32;
        let tx = bx + bw - pad - tw;
        let tlbl = "Include unstaged";
        let tlw = g.measure_chrome_text(tlbl, 13.0, false);
        g.draw_text(
            tx - 8.0 - tlw,
            my,
            tlbl,
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
        let on = git.commit_modal_include_unstaged;
        pill_rect(
            g,
            tx,
            my - 2.0,
            tw,
            th,
            if on {
                theme::accent()
            } else {
                theme::surface_active()
            },
        );
        let knob = th - 6.0;
        let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
        circle_rect(g, kx, my - 2.0 + 3.0, knob, [255, 255, 255, 255]);
        git.commit_modal_rects.push((
            GitModalBtn::IncludeUnstaged,
            (tx - 4.0, my - 5.0, tw + 8.0, th + 8.0),
        ));
        my += 28.0;
        // File list box
        let ctl_h = 32.0;
        let buttons = [
            ("Cancel", GitModalBtn::Cancel, false),
            ("Commit and push", GitModalBtn::CommitAndPush, false),
            ("Commit", GitModalBtn::Commit, true),
        ];
        let widths = buttons.map(|(label, _, primary)| native_controls::button_width(g, label, ctl_h, primary));
        let (footer, footer_h) = crate::native_layout::flow(
            &widths, (cx, 0.0, cw, 0.0), ctl_h, 8.0, crate::native_layout::Align::End,
        );
        let footer_y = bxy + bh - pad - footer_h;
        let input_y = footer_y - 12.0 - ctl_h;
        let lh = (input_y - 30.0 - my).max(0.0);
        g.push_clip(cx, my, cw, lh);
        panel_rect_outlined(g, cx, my, cw, lh, theme::radius_sm(), theme::surface());
        let nf = git_view.staged.len() + git_view.unstaged.len();
        let mut fx = g.draw_text(
            cx + 12.0,
            my + 10.0,
            &format!("{} files", nf),
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text(),
                bold: true,
                italic: false,
            },
        );
        fx = g.draw_text(
            fx + 10.0,
            my + 10.0,
            &format!("+{}", git_view.insertions),
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::success(),
                bold: false,
                italic: false,
            },
        );
        g.draw_text(
            fx + 8.0,
            my + 10.0,
            &format!("-{}", git_view.deletions),
            gpu::DrawOpts {
                font_size: 13.0,
                color: DIFF_RED,
                bold: false,
                italic: false,
            },
        );
        let mut ly = my + 34.0;
        for (_m, path) in git_view.staged.iter().chain(git_view.unstaged.iter()) {
            if ly > my + lh - 18.0 {
                break;
            }
            let fname = path.rsplit('/').next().unwrap_or(path.as_str());
            let dir = path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
            let ex = g.draw_text(
                cx + 12.0,
                ly,
                fname,
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: theme::text(),
                    bold: false,
                    italic: false,
                },
            );
            if !dir.is_empty() {
                g.draw_text(
                    ex + 7.0,
                    ly + 0.5,
                    dir,
                    gpu::DrawOpts {
                        font_size: 11.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            if let Some((ins, del)) = git_view.numstat.get(path) {
                let minus = format!("-{del}");
                let plus = format!("+{ins}");
                let wm = g.measure_chrome_text(&minus, 12.0, false);
                let wp = g.measure_chrome_text(&plus, 12.0, false);
                let mut rx = cx + cw - 12.0;
                if *del > 0 {
                    rx -= wm;
                    g.draw_text(
                        rx,
                        ly,
                        &minus,
                        gpu::DrawOpts {
                            font_size: 12.0,
                            color: DIFF_RED,
                            bold: false,
                            italic: false,
                        },
                    );
                    rx -= 6.0;
                }
                if *ins > 0 {
                    rx -= wp;
                    g.draw_text(
                        rx,
                        ly,
                        &plus,
                        gpu::DrawOpts {
                            font_size: 12.0,
                            color: theme::success(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            ly += 22.0;
        }
        g.pop_clip();
        my = input_y - 22.0;
        // Commit message box
        g.draw_text(
            cx,
            my,
            "Commit message",
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        my += 22.0;
        let inh = ctl_h;
        if git.commit_focused {
            round_rect(
                g,
                cx - 1.0,
                my - 1.0,
                cw + 2.0,
                inh + 2.0,
                theme::radius_sm(),
                theme::accent(),
            );
        }
        panel_rect(g, cx, my, cw, inh, theme::radius_sm(), theme::surface());
        g.push_clip(cx + 6.0, my, (cw - 12.0).max(0.0), inh);
        let itx = cx + 10.0;
        let ity = my + 9.0;
        let preedit = if git.commit_focused {
            preedit
        } else {
            ""
        };
        if git.commit_msg.is_empty() && preedit.is_empty() {
            g.draw_text(
                itx,
                ity,
                "변경 사항 설명…",
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
        }
        let cur = git.commit_cursor
            .min(git.commit_msg.chars().count());
        let (before, after) = crate::lineedit::split(&git.commit_msg, cur);
        let caret_width = g.measure_chrome_text(&before, 13.0, false)
            + g.measure_chrome_text(preedit, 13.0, false);
        let itx = itx - (caret_width - (cw - 20.0).max(0.0)).max(0.0);
        if let Some(range) = crate::lineedit::selection(crate::input::GIT_COMMIT_FIELD, cur, git.commit_msg.chars().count())
            .filter(|_| git.commit_focused && preedit.is_empty())
        {
            crate::lineedit::paint_band(g, &git.commit_msg, range, itx, ity, 13.0, false, cx + cw);
        }
        let mut px = g.draw_text(
            itx,
            ity,
            &before,
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        let caret_x = px;
        if !preedit.is_empty() {
            px = g.draw_text(
                px,
                ity,
                preedit,
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: theme::accent(),
                    bold: false,
                    italic: false,
                },
            );
        }
        if !after.is_empty() {
            g.draw_text(
                px,
                ity,
                &after,
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: theme::text(),
                    bold: false,
                    italic: false,
                },
            );
        }
        if git.commit_focused && preedit.is_empty() && commit_caret_on {
            g.rect(caret_x, ity, 1.5, 14.0, theme::text());
        }
        g.pop_clip();
        git.commit_input_rect = Some((cx, my, cw, inh));
        let can_commit = !git.commit_msg.trim().is_empty() || !preedit.trim().is_empty();
        for ((label, action, primary), (x, y, w, h)) in buttons.into_iter().zip(footer) {
            let enabled = action == GitModalBtn::Cancel || can_commit;
            let rect = native_controls::text_button(
                g, (x, y + footer_y, w, h), cursor, label,
                native_controls::Style { primary, enabled, ..Default::default() },
            );
            if enabled {
                if let Some(rect) = g.clip_hit(rect) { git.commit_modal_rects.push((action, rect)); }
            }
        }
        g.pop_clip();
    }
}
