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
    pub caret_on: bool,
    pub preedit: Option<&'a str>,
    pub quick: &'a [(String, Option<std::path::PathBuf>, &'static str)],
    pub active_file: Option<&'a std::path::Path>,
    pub cwd: Option<&'a std::path::Path>,
    pub git_badges: &'a std::sync::Mutex<HashMap<std::path::PathBuf, kasa_mcp::git::GitBadge>>,
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, tree: &mut state::FileTreeState, f: &Frame<'_>) {
    let (tree_col_x, tree_col_w, tree_col_bg) = (f.x, f.w, f.bg);

    let col_h = (f.viewport.1 - TITLE_HEIGHT).max(0.0);
    // Own background + right hairline so the column reads as a
    // distinct pane between the tabs and the cell grid.
    g.rect(
        tree_col_x,
        TITLE_HEIGHT,
        tree_col_w,
        col_h,
        tree_col_bg,
    );
    g.rect(
        tree_col_x + tree_col_w - 1.0,
        TITLE_HEIGHT,
        1.0,
        col_h,
        theme::border(),
    );
    let inset = SIDEBAR_TAB_INSET;
    let item_h = 26.0_f32;
    let row_x = tree_col_x + inset;
    let row_w = (tree_col_w - inset * 2.0).max(0.0);
    // Search box pinned to the column top; the tree starts below it.
    let tree_dens =
        Density::of(tree_col_w, FILE_TREE_DENSE_FULL, FILE_TREE_DENSE_COMPACT);
    let search_box_h = 28.0_f32;
    let sbx_y = TITLE_HEIGHT + 8.0;
    // Reserve room on the right for the new-folder / new-file
    // buttons; the search box takes what's left.
    let btn_sz = 24.0_f32;
    let btn_gap = 4.0_f32;
    // 좁아지면 새 폴더·새 파일 버튼을 접고 그 자리를 검색에 준다. 셋이
    // 자리를 나누던 때는 칼럼이 하한까지 밀리면 검색 상자가 58px 이 되어
    // 「검색…」 글자조차 안 들어갔다 — 파일을 **찾는** 길이 막히는 것이,
    // 만드는 버튼이 없는 것보다 크게 잃는다(만들기는 터미널에도 있다).
    let show_new_btns = tree_dens.at_least_compact();
    let buttons_w = if show_new_btns {
        btn_sz * 2.0 + btn_gap + 6.0
    } else {
        0.0
    };
    let search_w = (row_w - buttons_w).max(40.0);
    {
        let active = tree.search_active;
        let fill = if active {
            theme::surface_active()
        } else {
            theme::surface()
        };
        round_rect(
            g,
            row_x,
            sbx_y,
            search_w,
            search_box_h,
            theme::radius_sm(),
            theme::border(),
        );
        round_rect(
            g,
            row_x + 1.0,
            sbx_y + 1.0,
            search_w - 2.0,
            search_box_h - 2.0,
            theme::radius_sm() - 1.0,
            fill,
        );
        let ic = if active {
            theme::text()
        } else {
            theme::text_dim()
        };
        g.queue_icon(
            "folder-tree",
            row_x + 8.0,
            sbx_y + (search_box_h - 14.0) / 2.0,
            14.0,
            ic,
        );
        // 캐럿은 커서 자리다 — 늘 끝에 붙이면 가운데를 고치는 동안
        // 화면이 거짓말을 한다. 커서 앞뒤로 갈라 「앞 → 조합 중 글자
        // → 뒤」로 붙여 그리고, 캐럿은 그 앞부분 폭에 세운다.
        let (head, tail) = crate::lineedit::split(
            &tree.search_query,
            tree.search_cursor,
        );
        let mut head = head;
        if let Some(p) = f.preedit.filter(|_| active) {
            head.push_str(p);
        }
        let caret_w = g.measure_chrome_text(&head, 13.0, false);
        let shown = format!("{head}{tail}");
        let (txt, col) = if shown.is_empty() {
            ("검색…".to_string(), theme::text_mute())
        } else {
            (shown, theme::text())
        };
        g.draw_text(
            row_x + 30.0,
            sbx_y + (search_box_h - 13.0) / 2.0,
            &txt,
            gpu::DrawOpts {
                font_size: 13.0,
                color: col,
                bold: false,
                italic: false,
            },
        );
        // Blinking text caret when the box has focus.
        if active && f.caret_on {
            g.rect(
                row_x + 30.0 + caret_w,
                sbx_y + (search_box_h - 14.0) / 2.0,
                1.5,
                14.0,
                theme::text(),
            );
        }
        tree.search_rect = (row_x, sbx_y, search_w, search_box_h);
        // New-folder / new-file buttons.
        let (mx, my) = f.cursor;
        let bty = sbx_y + (search_box_h - btn_sz) / 2.0;
        let nf_x = row_x + search_w + 6.0;
        let nfile_x = nf_x + btn_sz + btn_gap;
        if show_new_btns {
            for (bx, icon) in [(nf_x, "folder-plus"), (nfile_x, "file-plus")] {
                let hover =
                    mx >= bx && mx <= bx + btn_sz && my >= bty && my <= bty + btn_sz;
                if hover {
                    hover_rect(g, bx, bty, btn_sz, btn_sz, theme::radius_sm());
                }
                let ic = if hover {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                g.queue_icon(
                    icon,
                    bx + (btn_sz - 15.0) / 2.0,
                    bty + (btn_sz - 15.0) / 2.0,
                    15.0,
                    ic,
                );
            }
            tree.new_folder_rect = (nf_x, bty, btn_sz, btn_sz);
            tree.new_file_rect = (nfile_x, bty, btn_sz, btn_sz);
        } else {
            // 안 그린 버튼은 히트렉트도 지운다 — 남겨 두면 검색 상자 위를
            // 눌렀을 때 보이지도 않는 버튼이 먼저 먹는다.
            tree.new_folder_rect = (0.0, 0.0, 0.0, 0.0);
            tree.new_file_rect = (0.0, 0.0, 0.0, 0.0);
        }
    }
    // Inline "new file/folder" naming row, pinned above the tree.
    let mut tree_top = sbx_y + search_box_h + 8.0;
    if let Some((is_dir, buf)) = tree.new.clone() {
        let iy = tree_top;
        round_rect(
            g,
            row_x,
            iy,
            row_w,
            item_h,
            theme::radius_sm(),
            theme::surface_active(),
        );
        g.rect(row_x, iy + 2.0, 2.0, item_h - 4.0, theme::accent());
        g.queue_icon(
            if is_dir { "folder" } else { "file" },
            row_x + 18.0,
            iy + (item_h - 16.0) / 2.0,
            16.0,
            theme::text(),
        );
        let (mut head, tail) = crate::lineedit::split(&buf, tree.edit_cursor);
        if let Some(p) = f.preedit {
            head.push_str(p);
        }
        let caret_w = g.measure_chrome_text(&head, 13.0, false);
        let shown = format!("{head}{tail}");
        let (txt, col) = if shown.is_empty() {
            (
                (if is_dir {
                    "폴더 이름…"
                } else {
                    "파일 이름…"
                })
                .to_string(),
                theme::text_mute(),
            )
        } else {
            (shown, theme::text())
        };
        g.draw_text(
            row_x + 44.0,
            iy + (item_h - 13.0) / 2.0,
            &txt,
            gpu::DrawOpts {
                font_size: 13.0,
                color: col,
                bold: false,
                italic: false,
            },
        );
        if f.caret_on {
            g.rect(
                row_x + 44.0 + caret_w,
                iy + (item_h - 14.0) / 2.0,
                1.5,
                14.0,
                theme::text(),
            );
        }
        tree.new_row_rect = (row_x, iy, row_w, item_h);
        tree_top += item_h;
    } else {
        tree.new_row_rect = (0.0, 0.0, 0.0, 0.0);
    }
    // File the focused pane is currently showing — its row gets an
    // active tint + accent bar so the sidebar tracks the open file.
    let active_file = f.active_file;
    // ── 빠른 파일 고정 섹션 ── 여기선 높이만 잡아 start_y 를 확정한다.
    // 그리기는 아래 원래 자리(트리 본문 앞)에서 한다 — 시저가 생기기
    // 전에는 스크롤로 start_y 위까지 올라온 트리 항목을 이 섹션의 불투명
    // 배경으로 **나중에 덮어야** 했고, 그 때문에 그리기만 뒤로 밀려 있었다.
    let quick = f.quick;
    let quick_top = tree_top;
    let quick_h = if quick.is_empty() {
        0.0
    } else {
        // 헤더(19) + 항목들(item_h*n) + 구분선(4+7)
        19.0 + quick.len() as f32 * item_h + 11.0
    };
    tree_top += quick_h;
    let start_y = tree_top;
    // 본문 geometry 를 스크롤 처리에 넘겨주기 위해 저장: start_y 는 검색박스
    // + 빠른파일 섹션(항목 수만큼 동적) 아래 첫 행 y, visible_h 는 상태줄을
    // 뺀 창 끝까지. input.rs 가 이걸로 max_scroll 을 정확히 clamp 한다.
    let bottom_h = f.status_h;
    let body_visible_h = (f.viewport.1 - bottom_h - start_y).max(0.0);
    tree.body_rect = (row_x, start_y, row_w, body_visible_h);
    let win_h = f.viewport.1;
    // 들여쓰기는 칼럼이 좁아질수록 줄인다. 고정 14px 이던 때는 깊은
    // 가지에서 `depth × step` 이 이름 자리를 통째로 먹어, 좁은 폭에서는
    // 파일 이름이 「…」 하나로 남았다 — 계층은 화살표와 아이콘으로도
    // 읽히니, 폭이 모자랄 때 가장 싸게 내줄 수 있는 것이 여기다.
    let step = match tree_dens {
        Density::Full => 14.0_f32,
        Density::Compact => 10.0,
        Density::Icon => 7.0,
    };
    let mut rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))> = Vec::new();
    // `file_tree_nodes` already holds the right set: a query swaps it
    // for whole-tree search hits (file_tree_search_collect), empty
    // restores the expanded tree. So just render it as-is.
    let vis_nodes: Vec<&FileNode> = tree.nodes.iter().collect();
    // ── 빠른 파일 고정 섹션 ── 트리 본문 **앞**에 그린다. 원래 자리다.
    //
    // 한동안 트리 뒤로 미뤄 뒀었는데, 이유는 레이아웃이 아니라 클리핑이
    // 없어서였다: 스크롤로 `start_y` 위까지 올라온 트리 항목을 막을 길이
    // 이 섹션의 불투명 배경으로 덮는 것뿐이었다. 이제 시저가 그 위를
    // 자르므로 덮을 것이 없고, 덮기를 위해 순서를 뒤집어 둘 이유도 없다.
    //
    // 순서를 되돌리는 편이 나은 이유: 「나중에 덮는다」는 배경이 불투명할
    // 때만 성립하는 약속이라, 이 섹션에 반투명 배경이나 둥근 모서리가
    // 붙는 순간 조용히 깨진다. 그리는 차례가 곧 z-order 인 편이 읽기도 쉽다.
    tree.quick_rects.clear();
    if !quick.is_empty() {
        g.rect(
            tree_col_x,
            quick_top,
            tree_col_w - 1.0,
            quick_h,
            tree_col_bg,
        );
        let mut qy = quick_top;
        g.draw_text(
            row_x + 6.0,
            qy + 3.0,
            "지침과 핸드오프",
            gpu::DrawOpts {
                font_size: 10.5,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        qy += 19.0;
        let (qmx, qmy) = f.cursor;
        for (label, path, icon) in quick {
            let y = qy;
            let hovered =
                qmx >= row_x && qmx <= row_x + row_w && qmy >= y && qmy <= y + item_h;
            let is_open = path.as_deref().is_some_and(|path| active_file.as_deref() == Some(path));
            if hovered && path.is_some() {
                hover_rect(g, row_x, y, row_w, item_h, theme::radius_sm());
            } else if is_open {
                round_rect(
                    g,
                    row_x,
                    y,
                    row_w,
                    item_h,
                    theme::radius_sm(),
                    theme::surface_active(),
                );
            }
            if is_open {
                g.rect(row_x, y + 2.0, 2.0, item_h - 4.0, theme::accent());
            }
            let isz = 16.0_f32;
            let iy = y + (item_h - isz) / 2.0;
            let icon_x = row_x + 18.0;
            let col = if path.is_none() {
                theme::text_mute()
            } else if hovered || is_open {
                theme::text()
            } else {
                theme::text_dim()
            };
            g.queue_icon(icon, icon_x, iy, isz, col);
            // 이 섹션은 트리 본문의 시저 **밖**에서 그려진다(고정 자리라
            // 스크롤에 안 걸린다). 잘라 줄 것이 없으니 좁은 트리에서는
            // 라벨이 칼럼을 넘어 터미널 위로 그대로 얹혔다(640px 실측).
            // 트리 본문 항목과 같은 규칙으로 여기서 재단한다.
            let ltx = icon_x + isz + 8.0;
            let lbl = crate::info::fit_text(
                g,
                label.as_str(),
                (row_x + row_w - 4.0 - ltx).max(0.0),
                13.0,
                false,
            );
            g.draw_text(
                ltx,
                y + (item_h - 13.0) / 2.0,
                &lbl,
                gpu::DrawOpts {
                    font_size: 13.0,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
            if let Some(path) = path {
                tree.quick_rects.push((path.clone(), (row_x, y, row_w, item_h)));
            }
            qy += item_h;
        }
        // 구분선 — 빠른 파일과 트리 본문 사이 하이라인.
        qy += 4.0;
        g.rect(
            row_x,
            qy,
            row_w,
            1.0,
            theme::with_alpha(theme::border(), 0x88),
        );
    }
    // git 표시는 **배지 폴러**가 채운 맵을 읽는다(git 컬럼 폴러가 아니라)
    // — 컬럼 폴러는 그 패널이 열렸을 때만 돌아서, 파일트리 표시가 남의
    // 패널 개폐에 묶여 버린다. 배지는 모든 pane 의 cwd 로 항상 돈다.
    // 루프 **밖에서 한 번만** 잠근다: 행마다 잠그면 폴러와 프레임당
    // 수십 번 부딪히고, 복사하면 프레임마다 맵을 통째로 clone 한다.
    let git_badges = f.git_badges.lock().ok();
    let git_marks = f
        .cwd
        .zip(git_badges.as_ref())
        .and_then(|(p, m)| m.get(p))
        .map(|b| &b.marks);
    // 트리 본문을 시저로 가둔다. 아래쪽 경계는 지금까지 컬링이 쓰던
    // `win_h` 그대로다 — 여기서 칼럼 끝(`view_bottom`)으로 좁히면 그 아래
    // 그려지던 행이 사라져 다른 변경이 섞인다. 위쪽만 진짜로 자른다.
    g.push_clip(tree_col_x, start_y, tree_col_w, (win_h - start_y).max(0.0));
    // 커서가 잘려 안 보이는 쪽에 있는데 행의 보이는 부분에 hover 배경이
    // 그려지는 것은 시저가 못 막는다 — hover 판정은 `file_tree.hover`
    // (마우스 이동 때 히트렉트로 정해진다)라 히트렉트를 자르면 함께 막힌다.
    for (idx, node) in vis_nodes.iter().enumerate() {
        let node = *node;
        let y = start_y - tree.scroll + idx as f32 * item_h;
        // 완전히 밖인 항목만 건너뛴다. 위로 반쯤 걸친 항목은 **그리고**
        // 시저가 자른다 — 예전엔 여기서 통째로 스킵했고, 그래야 했던 이유가
        // "덮어 줄 배경이 나중에 온다"였다. 잘라 낼 수 있게 된 지금은 반쯤
        // 걸친 행이 반쯤 보이는 것이 맞다.
        if !g.clip_visible(row_x, y, row_w, item_h) {
            continue;
        }
        let hovered = tree.hover.as_deref() == Some(node.path.as_path());
        let expanded = node.is_dir && tree.expanded.contains(&node.path);
        let is_open = active_file.as_deref() == Some(node.path.as_path());
        let is_selected = tree.selected.as_deref()
            == Some(node.path.as_path())
            || tree.selected_more.contains(&node.path);
        // Row background: hover wins; the open file / Cmd+Delete
        // selection keeps a solid active tint + accent bar; an open
        // folder keeps a faint tint so the branch reads as a group.
        if hovered {
            hover_rect(g, row_x, y, row_w, item_h, theme::radius_sm());
        } else if is_open || is_selected {
            round_rect(
                g,
                row_x,
                y,
                row_w,
                item_h,
                theme::radius_sm(),
                theme::surface_active(),
            );
        } else if expanded {
            round_rect(
                g,
                row_x,
                y,
                row_w,
                item_h,
                theme::radius_sm(),
                theme::with_alpha(theme::surface_hover(), 0x33),
            );
        }
        if is_open || is_selected {
            // Accent rail on the left edge — VSCode "active file" cue.
            g.rect(row_x, y + 2.0, 2.0, item_h - 4.0, theme::accent());
        }
        // Indent guides — one faint rule per ancestor level so deep
        // nesting stays legible.
        // 아주 좁을 때는 안내선을 접는다 — 폭이 132px 밑이면 세로줄
        // 여러 개가 이름보다 먼저 눈에 들어와, 계층을 돕는 것이 아니라
        // 목록을 읽기 어렵게 만든다.
        if tree_dens.at_least_compact() {
            for d in 0..node.depth {
                let gx = row_x + 6.0 + d as f32 * step;
                g.rect(gx, y, 1.0, item_h, theme::with_alpha(theme::border(), 0x55));
            }
        }
        let base_x = row_x + node.depth as f32 * step;
        let isz = 16.0_f32;
        let iy = y + (item_h - isz) / 2.0;
        let font = 13.0_f32;
        // Chevron column (folders only); files align past it.
        if node.is_dir {
            let chev = if expanded {
                "chevron-down"
            } else {
                "chevron-right"
            };
            let cc = if hovered {
                theme::text()
            } else {
                theme::text_mute()
            };
            g.queue_icon(chev, base_x + 2.0, y + (item_h - 12.0) / 2.0, 12.0, cc);
        }
        // 화살표와 아이콘 사이 간격도 폭을 따라 줄인다.
        let icon_x = base_x + if tree_dens.is_icon() { 13.0 } else { 18.0 };
        // Folders keep the single-color outline glyph (row-state
        // tint); files get the branded file-type icon (ft/*, full
        // color via FLAG_COLOR) with alpha carrying the ignored /
        // idle / hover states instead of a tint. Unknown types fall
        // back to the monochrome "file" glyph.
        let icon_color = if node.ignored {
            theme::with_alpha(theme::text_dim(), 0x99)
        } else if hovered || is_open {
            theme::text()
        } else {
            theme::text_dim()
        };
        if node.is_dir {
            // 레포는 폴더 대신 브랜치 아이콘 — 펼침 화살표가 이미
            // 폴더성을 말해 주므로 정보가 줄지 않고, 목록에서 어느
            // 게 레포인지 한눈에 갈린다(사용자).
            let ic = if node.is_repo { "git-branch" } else { "folder" };
            g.queue_icon(ic, icon_x, iy, isz, icon_color);
        } else if let Some(ft) = file_icon(&node.name) {
            let alpha = if node.ignored {
                0.35
            } else if hovered || is_open {
                1.0
            } else {
                0.85
            };
            g.queue_icon_colored(ft, icon_x, iy, isz, alpha);
        } else {
            g.queue_icon("file", icon_x, iy, isz, icon_color);
        }
        // Folders read brighter than files (soft hierarchy); ignored
        // rows are muted; hover/open lift to full strength.
        // git 마커가 있으면 이름 색이 그걸 따른다 — 배지만으론 좁은
        // 사이드바에서 눈에 안 들어온다(VSCode 도 이름을 물들인다).
        // ignored 행은 제외: gitignore 된 것은 애초에 status 에 안 나오고,
        // 나온다 해도 흐리게 두는 게 이 행의 뜻이다.
        let git_mark = (!node.ignored)
            .then(|| git_marks.and_then(|m| m.get(&node.path).copied()))
            .flatten();
        let mark_color = |m: char| match m {
            'M' => theme::syn_type(),
            'A' | 'U' => theme::success(),
            'D' => theme::danger(),
            _ => theme::text_dim(),
        };
        let fg = if let Some(m) = git_mark {
            mark_color(m)
        } else if node.ignored {
            theme::text_mute()
        } else if hovered || is_open || node.is_dir {
            theme::text()
        } else {
            theme::text_dim()
        };
        let text_x = icon_x + isz + 8.0;
        // Clip the name to the column width with an ellipsis — long
        // hashed file names (webp/jpg) otherwise overflow the sidebar
        // straight into the terminal grid.
        // 배지 자리를 먼저 떼어 둔다 — 안 그러면 긴 이름이 배지 밑을
        // 파고들어 글자와 마커가 겹친다.
        let badge_w = if git_mark.is_some() { 13.0 } else { 0.0 };
        let avail = (row_x + row_w - text_x - 4.0 - badge_w).max(0.0);
        let label = if g.measure_chrome_text(&node.name, font, false) <= avail {
            node.name.clone()
        } else {
            let mut s = String::new();
            for ch in node.name.chars() {
                let mut trial = s.clone();
                trial.push(ch);
                trial.push('…');
                if g.measure_chrome_text(&trial, font, false) > avail {
                    break;
                }
                s.push(ch);
            }
            s.push('…');
            s
        };
        // Inline rename: this row's name turns into an edit box with
        // a caret instead of the static label (same input path as the
        // new-file/folder row).
        let editing = tree
            .rename
            .as_ref()
            .filter(|(p, _)| p == &node.path)
            .map(|(_, n)| n.clone());
        if let Some(name) = editing {
            let (mut head, tail) =
                crate::lineedit::split(&name, tree.edit_cursor);
            if let Some(p) = f.preedit {
                head.push_str(p);
            }
            let caret_w = g.measure_chrome_text(&head, font, false);
            let shown = format!("{head}{tail}");
            let (txt, tcol) = if shown.is_empty() {
                ("이름…".to_string(), theme::text_mute())
            } else {
                (shown, theme::text())
            };
            g.draw_text(
                text_x,
                y + (item_h - font) / 2.0,
                &txt,
                gpu::DrawOpts {
                    font_size: font,
                    color: tcol,
                    bold: false,
                    italic: false,
                },
            );
            if f.caret_on {
                g.rect(
                    text_x + caret_w,
                    y + (item_h - 14.0) / 2.0,
                    1.5,
                    14.0,
                    theme::text(),
                );
            }
            tree.rename_row_rect = (row_x, y, row_w, item_h);
        } else {
            g.draw_text(
                text_x,
                y + (item_h - font) / 2.0,
                &label,
                gpu::DrawOpts {
                    font_size: font,
                    color: fg,
                    bold: false,
                    italic: node.ignored,
                },
            );
            // 행 오른쪽 끝 상태 배지. 파일은 글자(M/A/U)로 무엇이
            // 바뀌었는지까지 말하고, 폴더는 점 하나 — 폴더의 글자는
            // 자손 여럿을 하나로 뭉친 것이라 "M" 이라 쓰면 그 폴더가
            // 수정됐다는 오해를 준다. 점은 "안에 뭔가 있다"만 말한다.
            if let Some(m) = git_mark {
                let c = mark_color(m);
                if node.is_dir {
                    let d = 6.0_f32;
                    round_rect(
                        g,
                        row_x + row_w - 4.0 - d,
                        y + (item_h - d) / 2.0,
                        d,
                        d,
                        d / 2.0,
                        c,
                    );
                } else {
                    let bf = 10.0_f32;
                    let bw = g.measure_chrome_text(&m.to_string(), bf, true);
                    g.draw_text(
                        row_x + row_w - 4.0 - bw,
                        y + (item_h - bf) / 2.0,
                        &m.to_string(),
                        gpu::DrawOpts {
                            font_size: bf,
                            color: c,
                            bold: true,
                            italic: false,
                        },
                    );
                }
            }
        }
        // 눌리는 자리는 클립과의 교집합이다. 시저는 픽셀만 자르지 클릭은
        // 안 자르므로, 원본을 담으면 「빠른 파일」 뒤로 스크롤된 행이
        // 그대로 눌린다 — 예전 통째 스킵이 막아 주던 것이 바로 이것이고,
        // 스킵을 지웠으니 여기서 대신 막아야 한다.
        if let Some(hr) = g.clip_hit((row_x, y, row_w, item_h)) {
            rects.push((node.path.clone(), hr));
        }
    }
    g.pop_clip();
    tree.rects = rects;

    // Overflow affordances: a soft fade at whichever edge still has
    // hidden rows, plus a hover-only scrollbar thumb. The viewport
    // runs from the first row (`start_y`, already below the search
    // box / inline new-row) to the column bottom, so the fade never
    // eats the chrome above it.
    let view_top = start_y;
    let view_bottom = TITLE_HEIGHT + col_h;
    let viewport_h = (view_bottom - view_top).max(0.0);
    let content_h = tree.nodes.len() as f32 * item_h;
    if content_h > viewport_h + 0.5 {
        let overflow = content_h - viewport_h;
        let scroll = tree.scroll;
        let fade_h = 28.0_f32;
        let strips = 16;
        let strip_h = fade_h / strips as f32 + 0.5;
        // Top fade ramps in over the first `fade_h` of scroll so it
        // appears gently instead of snapping on at the first pixel.
        if scroll > 0.5 {
            let k = (scroll / fade_h).min(1.0);
            for i in 0..strips {
                let t = i as f32 / (strips - 1) as f32; // 0 top → 1 bottom of band
                let a = ((1.0 - t) * 0.92 * k * 255.0) as u8;
                g.rect(
                    tree_col_x,
                    view_top + t * fade_h,
                    tree_col_w - 1.0,
                    strip_h,
                    theme::with_alpha(theme::bg(), a),
                );
            }
        }
        // Bottom fade — rows still hidden below the last visible line.
        if scroll < overflow - 0.5 {
            let k = ((overflow - scroll) / fade_h).min(1.0);
            for i in 0..strips {
                let t = i as f32 / (strips - 1) as f32; // 0 top → 1 bottom of band
                let a = (t * 0.92 * k * 255.0) as u8;
                g.rect(
                    tree_col_x,
                    view_bottom - fade_h + t * fade_h,
                    tree_col_w - 1.0,
                    strip_h,
                    theme::with_alpha(theme::bg(), a),
                );
            }
        }
        // Scrollbar thumb — only while the cursor hovers the column,
        // so the chrome stays clean when you're reading, not scrolling.
        let (mx, my) = f.cursor;
        let over_col = mx >= tree_col_x
            && mx < tree_col_x + tree_col_w
            && my >= view_top
            && my < view_bottom;
        if over_col {
            let thumb_h = (viewport_h * viewport_h / content_h).max(28.0);
            let thumb_y =
                view_top + (viewport_h - thumb_h) * (scroll / overflow).clamp(0.0, 1.0);
            pill_rect(
                g,
                tree_col_x + tree_col_w - 6.0,
                thumb_y,
                3.5,
                thumb_h,
                theme::with_alpha(theme::text(), 0x66),
            );
        }
    }
    // Right-click context menu — painted last in the column so it
    // overlays the rows. Items + hit rects build straight into
    // `ctx_menu_rects`.
    tree.ctx_menu_rects.clear();
    if let Some((rawx, rawy)) = tree.ctx_menu {
        let sel_n = tree.selected_more.len()
            + tree.selected.is_some() as usize;
        let del_label = if sel_n > 1 {
            format!("{sel_n}개 삭제")
        } else {
            "휴지통으로 삭제".to_string()
        };
        #[cfg(target_os = "macos")]
        let reveal_label = "Finder에서 보기";
        #[cfg(not(target_os = "macos"))]
        let reveal_label = "탐색기에서 보기";
        // (action, label, danger, separator-before). "…에서 열기"는
        // 설치된 앱 수만큼 늘어나므로 배열이 아니라 Vec 이다.
        // "…로/으로" 대신 "…에서"로 통일한 건 조사 때문 — 영문
        // 앱 이름은 받침 판정이 안 서고, "Finder에서 보기"와도
        // 어울린다.
        let mut items: Vec<(crate::FtMenuAction, String, bool, bool)> = Vec::new();
        for (i, (name, _)) in crate::proc::open_with_apps().iter().enumerate() {
            items.push((
                crate::FtMenuAction::OpenWith(i),
                format!("{name}에서 열기"),
                false,
                false,
            ));
        }
        items.push((
            crate::FtMenuAction::OpenDefault,
            "기본 앱으로 열기".to_string(),
            false,
            false,
        ));
        let first_sep = !items.is_empty();
        items.extend([
            (
                crate::FtMenuAction::NewFile,
                "새 파일".to_string(),
                false,
                first_sep,
            ),
            (
                crate::FtMenuAction::NewFolder,
                "새 폴더".to_string(),
                false,
                false,
            ),
            (
                crate::FtMenuAction::Rename,
                "이름 변경".to_string(),
                false,
                true,
            ),
            (
                crate::FtMenuAction::CopyPath,
                "경로 복사".to_string(),
                false,
                false,
            ),
            (
                crate::FtMenuAction::Reveal,
                reveal_label.to_string(),
                false,
                false,
            ),
            (crate::FtMenuAction::Delete, String::new(), true, true),
        ]);
        let mih = 28.0_f32;
        let sep = 7.0_f32;
        let pad = 6.0_f32;
        // 폭은 가장 긴 항목에 맞춘다. 고정 200px 이던 시절엔 앱
        // 이름이 긴 항목("Antigravity에서 열기")이 오른쪽 테두리를
        // 넘어 잘렸다 — 항목이 기기마다 다르니 폭도 그래야 한다.
        let widest = items
            .iter()
            .map(|(action, label, _, _)| {
                let s = if matches!(action, crate::FtMenuAction::Delete) {
                    del_label.as_str()
                } else {
                    label.as_str()
                };
                g.measure_chrome_text(s, 13.0, false)
            })
            .fold(0.0_f32, f32::max);
        let menu_w = (widest + 32.0).max(200.0);
        let nsep = items.iter().filter(|(_, _, _, s)| *s).count() as f32;
        let menu_h = pad * 2.0 + items.len() as f32 * mih + nsep * sep;
        let win_w = f.viewport.0;
        let mx = rawx.min(win_w - menu_w - 6.0).max(tree_col_x + 2.0);
        let my = rawy.min(win_h - menu_h - 6.0).max(TITLE_HEIGHT + 2.0);
        panel_rect_outlined(
            g,
            mx,
            my,
            menu_w,
            menu_h,
            theme::radius_md(),
            theme::surface(),
        );
        let (curx, cury) = f.cursor;
        let mut iy = my + pad;
        for (action, label, danger, sep_before) in items {
            if sep_before {
                g.rect(
                    mx + pad,
                    iy + sep * 0.5,
                    menu_w - pad * 2.0,
                    1.0,
                    theme::with_alpha(theme::border(), 0x88),
                );
                iy += sep;
            }
            let r = (mx + 4.0, iy, menu_w - 8.0, mih);
            let hov =
                curx >= r.0 && curx <= r.0 + r.2 && cury >= r.1 && cury <= r.1 + r.3;
            if hov {
                hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm());
            }
            let lbl = if matches!(action, crate::FtMenuAction::Delete) {
                del_label.as_str()
            } else {
                label.as_str()
            };
            let color = if danger {
                theme::danger()
            } else {
                theme::text()
            };
            g.draw_text(
                r.0 + 12.0,
                r.1 + (mih - 13.0) / 2.0,
                lbl,
                gpu::DrawOpts {
                    font_size: 13.0,
                    color,
                    bold: false,
                    italic: false,
                },
            );
            tree.ctx_menu_rects.push((action, r));
            iy += mih;
        }
    }
}
