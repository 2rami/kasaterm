//! 배치와 히트렉트는 GPU 를 빌리기 전에 `sidebar_layout` 이 정한다. 여기서 자리를 다시
//! 계산하면 그린 곳과 눌리는 곳이 갈리므로 같은 숫자로 그리기만 한다.
use super::*;

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy)]
pub(super) struct Strip {
    pub w: f32,
    pub view: (f32, f32),
    pub cursor: (f32, f32),
}

/// `tabs` 를 뺀 나머지는 방 인덱스로 늘어놓은 것이다.
#[derive(Clone, Copy)]
pub(super) struct Rooms<'a> {
    pub tabs: &'a [(usize, Rect)],
    pub labels: &'a [(String, String)],
    pub active: usize,
    pub hover: Option<usize>,
    pub numbers: &'a [Option<usize>],
    pub wait: &'a [bool],
    pub wait_loud: &'a [bool],
    pub alert: &'a [bool],
    /// 펼친 줄이 이미 대기·알림을 말하고 있는 방 — 그 카드 머리는 조용히 둔다.
    pub row_wait: &'a std::collections::HashSet<usize>,
    pub row_alert: &'a std::collections::HashSet<usize>,
    pub expand: &'a [Option<Rect>],
    pub expand_t: &'a [f32],
    pub dots: &'a [Vec<[u8; 4]>],
    pub closes: &'a [(usize, Rect)],
}

#[derive(Clone, Copy)]
pub(super) struct Panes<'a> {
    pub rows: &'a [(usize, String, Rect)],
    pub row_info: &'a [SidebarRowInfo],
    pub mini: &'a [(usize, String, Rect)],
    pub mini_info: &'a [SidebarRowInfo],
    pub undock: &'a [(usize, String, Rect)],
    pub undock_info: &'a [SidebarRowInfo],
    pub active_pane: Option<&'a str>,
    /// 끌고 있는 줄 `(대상, 자리, 끌리는 줄)`.
    pub row_drop: Option<&'a (String, crate::DropZone, String)>,
}

#[derive(Clone, Copy)]
pub(super) struct Edges<'a> {
    pub tabs: &'a [(usize, Rect)],
    pub plus: Rect,
    pub over_before: bool,
    pub over_after: bool,
    pub scroll: Option<(f32, f32, f32, f32)>,
    pub drag_target: Option<usize>,
    pub menu_open: bool,
    pub tray: Option<&'a crate::chrome::SidebarTray>,
}

pub(super) struct RowMenu<'a> {
    pub anchor: (f32, f32),
    /// 빈 문자열이면 방 메뉴다.
    pub pane: &'a str,
    pub room_is_list: bool,
    pub pulse_label: &'a str,
    pub hidden: bool,
    pub undocked: bool,
    pub weather_on: bool,
    pub weather_now: crate::weather::model::PaneWeather,
}


pub(super) fn paint_rooms(g: &mut gpu::GpuRenderer, s: &Strip, r: &Rooms<'_>) {
    let Strip { w: tab_strip_w, view: sb_view, cursor: sb_cursor } = *s;
    let Rooms {
        tabs: sb_tabs,
        labels: sb_labels,
        active: sb_active,
        hover: sb_hover,
        numbers: sb_room_numbers,
        wait: sb_wait,
        wait_loud: sb_wait_loud,
        alert: sb_alert,
        row_wait: sb_row_wait_win,
        row_alert: sb_row_alert_win,
        expand: sb_expand,
        expand_t: sb_expand_t,
        dots: sb_dots,
        closes: sb_closes,
    } = *r;
    // 칼럼 바닥과 오른쪽 실선은 위 크롬 판에서 한 번에 칠했다.
    let multi = sb_tabs.len() > 1;
    // 잘라 그린다 — 픽셀로 흐르니 카드가 칸 경계에 반쯤 걸친 상태가
    // 정상이다. 시저가 없으면 그 반쪽이 타이틀바와 트레이 위로 그대로
    // 삐져나온다(사이드바는 원래 클립을 안 세웠다).
    g.push_clip(0.0, sb_view.0, tab_strip_w, sb_view.1);
    for (i, (tx, ty, tw, th)) in sb_tabs {
        let is_active = *i == sb_active;
        let is_hover = sb_hover == Some(*i);
        // Selected tab: subtle rounded highlight box (no left
        // accent bar). Non-selected: flat, only a faint box on
        // hover. Warp-style.
        g.hover_pointer |= is_hover;
        if is_active {
            // 고른 방은 **테두리로만** 말한다(2026-08-11 지시: "선택한 방
            // 아웃라인으로 포커스 바꾸고"). 채운 판이었을 땐 그 밝은
            // 바탕이 카드 전체를 덮어, 그 위에 얹히는 상태색(알림 점·
            // 대기 주황)이 같은 밝기 대역에서 겨뤘다 — 정작 봐야 할
            // 신호가 "고름"에 묻혔다. 테두리는 자리만 두르고 안을 비운다.
            //
            // 되메우는 색이 `panel_bg` 인 건 이 스트립의 바탕이 그것이기
            // 때문이다(이 함수 위쪽에서 칼럼째 칠한다). 링만 그리는
            // 스트로크가 렌더러에 없어 안쪽을 바탕색으로 덮는 방식이다.
            // 2026-09-14 플랫 개편: 설정·보드의 목록과 같은 문법으로
            // 고른 방은 **은은한 판**(surface_active)이다. 링은 목록에서
            // 유일하게 튀는 선이라 카드 셋 중 하나만 액자처럼 보였다.
            panel_rect(
                g,
                *tx,
                *ty,
                *tw,
                *th,
                theme::radius_md(),
                theme::surface_active(),
            );
        } else if is_hover {
            panel_rect(
                g,
                *tx,
                *ty,
                *tw,
                *th,
                theme::radius_md(),
                theme::surface_hover(),
            );
        }
        // 방과 방 사이 실선. 활성·호버 카드만 판을 깔기 때문에, 조용한
        // 방끼리는 3px 틈만 있고 경계가 없었다 — 두 줄짜리 카드가 죽
        // 이어지면 어디까지가 한 방인지 안 읽힌다(사용자: "구분선이 하나도
        // 없어"). 활성 카드는 스스로 판이라 그 위아래엔 긋지 않는다.
        if !is_active && *i + 1 < sb_tabs.len() && *i + 1 != sb_active {
            let ly = (ty + th + SIDEBAR_TAB_GAP / 2.0).round();
            g.rect(
                tx + 10.0,
                ly,
                tw - 20.0,
                1.0,
                theme::with_alpha(theme::border(), 0x60),
            );
        }
        let (name, cwd) = sb_labels
            .get(*i)
            .cloned()
            .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
        // 이름 왼쪽은 **상태 점 한 칸**이다. 예전엔 여기 터미널 글리프가
        // 있었는데 `tab_icon_glyph` 은 이름이 `.md` 로 끝날 때만 갈리고
        // 방 이름은 폴더명이라 그럴 일이 없어서, 모든 방이 같은 글리프를
        // 달고 있었다 — 아무것도 안 가르면서 이름을 32px 밀었다
        // (2026-08-24 지시: "방에서 >_ 이표시는 없어도되지않아?").
        //
        // 점이 없어도 이 칸은 비워 둔다. 점이 뜬 카드만 이름이 밀리면
        // 목록의 왼쪽 정렬이 흔들리고, 방 목록은 훑는 화면이라 그 흔들림이
        // 곧 읽는 속도다.
        // 카드가 아니라 **스트립** 폭으로 판정한다(카드는 좌우 inset 만큼
        // 좁다) — 문턱은 칼럼 폭 기준으로 잡혀 있다.
        let sb_dens = Density::of(
            *tw + SIDEBAR_TAB_INSET * 2.0,
            SIDEBAR_DENSE_FULL,
            SIDEBAR_DENSE_COMPACT,
        );
        // 가장 좁을 때는 그 여백을 이름에 넘긴다. 점이 뜨는 것은 예외
        // 상황인데 그 자리를 늘 비워 두느라 「Desktop」이 「Desk…」가
        // 됐다 — 정렬이 흔들리는 것보다 이름을 못 읽는 쪽이 크다.
        let gutter = if sb_dens.is_icon() { 6.0 } else { 12.0 };
        let dot_gap = if sb_dens.is_icon() { 3.0 } else { 5.0 };
        let dot_x = *tx + gutter;
        let dot_y = *ty + 13.0;
        let dsz = 9.0_f32;
        // 상태 동그라미 **하나**. 예전엔 칩의 두 모서리에 점이 따로
        // 있었다 — 작업 중은 오른쪽 위, 방금 끝남은 오른쪽 아래. 상태가
        // 바뀔 때마다 점이 두 모서리를 오갔고, 그 움직임이 정작 무엇이
        // 달라졌는지보다 먼저 눈에 들어왔다(2026-08-11 지시: "점 두개
        // 왔다갔다거리는거 없애자"). 이제 자리는 하나로 고정하고 **색이**
        // 무엇인지 말한다.
        //
        // 그리는 조건도 갈렸다. "작업 중"은 여기서 뺐다 — 펼친 방의 줄이
        // 학생을 걷게 해서 이미 말하고 있고(아래 pane 줄), 카드가 그걸 또
        // 말하면 같은 정보가 두 층에 겹친다. 여기 남는 건 **내 손이
        // 필요한 것**뿐이라 늘 깜빡인다.
        let wait = sb_wait.get(*i).copied().unwrap_or(false);
        let alert = sb_alert.get(*i).copied().unwrap_or(false);
        // 줄이 이미 말하고 있으면 머리는 조용히 둔다 — 접힌 방은 줄이
        // 없어 여기가 유일한 자리다.
        let head_wait = wait && !sb_row_wait_win.contains(i);
        let head_alert = alert && !sb_row_alert_win.contains(i);
        if head_wait || head_alert {
            // 대기가 이긴다: 끝나서 알리는 것과 막혀서 부르는 것은 급한
            // 정도가 다르고, `handle_attention` 이 unread 에도 넣기 때문에
            // 둘은 자주 같이 선다.
            let (c, period) = if head_wait {
                (theme::attention(), 0.9)
            } else {
                (theme::accent(), 1.6)
            };
            if head_wait && !sb_wait_loud.get(*i).copied().unwrap_or(false) {
                circle_rect(g, dot_x, dot_y, dsz, c);
            } else {
                blink_dot(g, dot_x, dot_y, dsz, c, period);
            }
        }
        // 두 줄짜리 라벨 — 상태 점 칸 오른쪽.
        let text_x = dot_x + dsz + dot_gap;
        let name_fg: [u8; 4] = if is_active {
            theme::text()
        } else {
            theme::text_dim()
        };
        // 경로는 방을 가르는 유일한 단서일 때가 많다(이름이 죄다
        // 폴더명이라 같아진다). `text_mute` 는 "있지만 안 읽어도 되는
        // 것"의 톤이라 여기선 너무 물러나 있었다 — 한 단 올린다.
        let cwd_fg: [u8; 4] = theme::text_dim();
        // × 는 **hover 에만** 낸다. 활성 방에도 상시로 내던 때는 아래
        // `⌘N` 배지와 같은 칸을 다퉈, 하필 **지금 있는 방일수록 번호가
        // 안 보였다**(2026-08-27 지적). 번호는 곧 단축키(Cmd+숫자)라 활성
        // 방에서 가장 알고 싶은 값이고, 닫기는 마우스를 얹으면 그 자리에
        // 나타나므로 잃는 것이 없다 — 누르러 가는 동안 사라지는 반대
        // 경우(되돌리기 버튼)와 달리, 이건 다가갈수록 나타난다.
        let show_close = multi && is_hover;
        // Budgets are measured against the tab's own right edge, not
        // the sidebar width: the label starts at `text_x` (inset +
        // ordinal gutter + chip), so a sidebar-width budget overshoots
        // by the inset at both ends and ran the name under the ×.
        // The name shares its row with the × and reserves that slot
        // (close box is 14 wide, 3 from the edge, +6 breathing room);
        // the cwd line sits below the × and gets the width back.
        let tab_right = *tx + *tw;
        // 창 번호는 곧 단축키다(Cmd+숫자, input.rs `win_digit`) — 맨
        // 숫자를 왼쪽 여백에 두면 "몇 번째"까지만 말하지만, 이름 옆의
        // `⌘1` 은 "이 키로 온다"까지 말한다. 9 까지만 매핑돼 있고, ×
        // 가 뜨는 동안은 같은 자리라 물러난다.
        // 아주 좁을 때는 번호를 접는다. 남는 폭이 40px 남짓인데 배지가
        // 그 절반을 가져가면 방 이름이 한 글자도 안 남아, 「어느 방인가」를
        // 못 읽는다 — 단축키는 못 봐도 눌러지지만 이름은 안 보이면 끝이다.
        let kbd = sb_room_numbers.get(*i).copied().flatten()
            .filter(|n| !show_close && *n < 9 && sb_dens.at_least_compact())
            .map(|n| format!("\u{2318}{}", n + 1));
        let kfs = 11.0_f32;
        let kbd_w = kbd
            .as_deref()
            .map_or(0.0, |k| g.measure_chrome_text(k, kfs, false));
        let right_slot = kbd_w;
        // 배지를 접었으면 그 자리도 함께 돌려준다 — 안 돌려주면 접어서
        // 번 폭이 오른쪽 여백에 그대로 남아, 정작 이름은 그대로 잘린다.
        let right_pad = if show_close {
            23.0
        } else if right_slot > 0.0 {
            8.0 + right_slot + 6.0
        } else {
            6.0
        };
        let name_budget = (tab_right - right_pad - text_x).max(0.0);
        // 아랫줄 오른쪽은 이제 펼치기 배지 몫이다. 여기 있던 pane 별
        // 상태 점은 뺐다 — 방 목록은 "어느 방으로 갈까"를 고르는 자리고,
        // pane 하나하나의 상태는 방을 펴면 그 줄이 이미 말한다. 둘 다
        // 두면 같은 정보가 두 층에 겹쳐 목록이 시끄러워진다(사용자:
        // "학생 목록 말고 윈도우 목록에선 없애").
        let badge_w = sb_expand
            .get(*i)
            .copied()
            .flatten()
            .map_or(0.0, |r| r.2 + 14.0);
        let cwd_budget = (tab_right - 8.0 - badge_w - text_x).max(0.0);
        // 경로 줄이 서는가 — 아래 cwd 분기와 같은 조건이어야 한다.
        // 내부 방(설정·보드)은 경로가 없어 이름을 윗줄에 두면 카드 아래
        // 절반이 빈 상자로 남았다. 줄이 없으면 이름을 카드 가운데로.
        let cwd_shown = !cwd.is_empty() && sb_dens.at_least_compact();
        let name_y = if cwd_shown {
            *ty + 11.0
        } else {
            *ty + ((SIDEBAR_TAB_H - 17.0) / 2.0).round()
        };
        // Clip before drawing — `draw_text` also borrows `g`.
        let name_txt = clip_px(g, &name, 13.5, is_active, name_budget);
        g.draw_text(
            text_x,
            name_y,
            &name_txt,
            gpu::DrawOpts {
                font_size: 13.5,
                color: name_fg,
                bold: is_active,
                italic: false,
            },
        );
        if let Some(k) = kbd {
            g.draw_text(
                tab_right - 8.0 - kbd_w,
                name_y + 1.0,
                &k,
                gpu::DrawOpts {
                    font_size: kfs,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
        }
        // 경로는 방을 가르는 좋은 단서지만, 「…」 몇 글자로 잘리면
        // 가르지도 못하면서 이름과 자리만 다툰다. 그 폭에서는 아예 접고
        // 이름을 카드 가운데로 놓는다.
        if cwd_shown {
            let cwd_txt = clip_px(g, &cwd, 11.0, false, cwd_budget);
            g.draw_text(
                text_x,
                *ty + 30.0,
                &cwd_txt,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: cwd_fg,
                    bold: false,
                    italic: false,
                },
            );
        }
        // 펼치기 배지 — 삼각형 + pane 개수. 방 전환의 유일한 예외라
        // 평소에도 테두리를 둘러 "여긴 버튼"이라고 말해 둔다. 사각은
        // 클릭 판정과 같은 것을 쓴다(`window_expand_rect`).
        if let Some(er) = sb_expand.get(*i).copied().flatten() {
            let hov = sb_cursor.0 >= er.0
                && sb_cursor.0 <= er.0 + er.2
                && sb_cursor.1 >= er.1
                && sb_cursor.1 <= er.1 + er.3;
            g.hover_pointer |= hov;
            // 밝기는 **이 카드 위에서** 정해진다. 고정 톤을 쓰면 활성
            // 카드(더 밝은 판) 위에서 들리기는커녕 되레 어두워졌다.
            // 플랫 문법(2026-09-14): 배지는 판 없이 글자·화살표만.
            // 눌릴 자리라는 건 hover 판이 말한다.
            if hov {
                hover_rect(g, er.0, er.1, er.2, er.3, theme::radius_sm());
            }
            // 화살표는 목록을 여는 유일한 표지라 배지 안에서 가장 밝아야
            // 한다. `text_dim` 은 부제 색이어서, 10px 로 그린 화살표가
            // 배지 채움에 묻혀 숫자만 떠 있는 칩으로 보였다(2026-08-27 지적).
            let fg = if hov {
                theme::text()
            } else {
                theme::lerp(theme::text_dim(), theme::text(), 0.55)
            };
            // 삼각형은 목록이 절반 열렸을 때 넘어간다 — 누르자마자
            // 바뀌면 아직 닫힌 목록 위에서 이미 열린 표시가 된다.
            g.queue_icon(
                if sb_expand_t.get(*i).copied().unwrap_or(0.0) >= 0.5 {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                er.0 + 5.0,
                er.1 + 3.0,
                14.0,
                fg,
            );
            let n = sb_dots.get(*i).map_or(0, |v| v.len()).to_string();
            g.draw_text(
                er.0 + 21.0,
                er.1 + 5.0,
                &n,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: fg,
                    bold: false,
                    italic: false,
                },
            );
        }
        // × close — only on the active or hovered tab (where the
        // cursor is), so the strip stays clean otherwise. Hit
        // rects exist for every tab; you hover before you click.
        if show_close {
            if let Some((_, (cx, cy, cw, ch))) =
                sb_closes.iter().find(|(ci, _)| ci == i)
            {
                // Hover chip behind the × — same lift the pane-header
                // close gets, so the sidebar close reads as clickable.
                let x_hover = sb_cursor.0 >= *cx
                    && sb_cursor.0 <= *cx + *cw
                    && sb_cursor.1 >= *cy
                    && sb_cursor.1 <= *cy + *ch;
                if x_hover {
                    hover_rect(g, *cx, *cy, *cw, *ch, theme::radius_sm());
                }
                let xcol = if x_hover {
                    theme::text()
                } else {
                    theme::text_mute()
                };
                g.queue_icon(
                    "x",
                    *cx + (*cw - theme::ICON_SIZE) / 2.0,
                    *cy + (*ch - theme::ICON_SIZE) / 2.0,
                    theme::ICON_SIZE,
                    xcol,
                );
            }
        }
    }
    g.pop_clip();
}

/// 팝업은 여기서 그리지 않고 `(별도창, 배치도)` 로 돌려준다 — 파일트리·git 칼럼이 뒤에
/// 그려져 그 위를 덮으므로 칼럼들 뒤에 한 번 그린다.
pub(super) fn paint_layouts(
    g: &mut gpu::GpuRenderer,
    s: &Strip,
    p: &Panes<'_>,
) -> (Option<(f32, f32, String)>, Option<(f32, f32, Vec<TabPeek>)>) {
    let Strip { w: tab_strip_w, view: sb_view, cursor: sb_cursor } = *s;
    let Panes {
        mini: sb_mini,
        mini_info: sb_mini_info,
        undock: sb_undock,
        undock_info: sb_undock_info,
        active_pane: sb_active_pane,
        row_drop: sb_row_drop,
        ..
    } = *p;
    // 방 배치도 — 목록보다 **먼저** 그린다(행 hover 판이 위에 와야 한다).
    // 목록은 "누가 있나"만 말하고 어느 칸이 화면 어디인지는 못 말한다.
    // 덱 위에 마우스가 있을 때 펼 명단. 루프 **밖에서** 그린다 — 안에서
    // 그리면 뒤에 오는 칸이 위에 얹혀 팝업이 잘린다.
    let mut deck_tip: Option<(f32, f32, Vec<TabPeek>)> = None;
    // 잘라 그린다 — 픽셀로 흐르니 카드가 칸 경계에 반쯤 걸친 상태가
    // 정상이다. 시저가 없으면 그 반쪽이 타이틀바와 트레이 위로 그대로
    // 삐져나온다(사이드바는 원래 클립을 안 세웠다).
    g.push_clip(0.0, sb_view.0, tab_strip_w, sb_view.1);
    for ((_, id, r), info) in sb_mini.iter().zip(sb_mini_info.iter()) {
        let (mx, my, mw, mh) = *r;
        let cur = sb_active_pane.as_deref() == Some(id.as_str());
        // 히트 판정은 **레이아웃이 준 칸 그대로** 본다 — 아래에서 덱만큼
        // 줄이는 건 그림일 뿐이고, 클릭/드래그는 `sb_hits` 에 들어간 원래
        // 사각으로 판정된다. 여기만 줄이면 손과 눈이 어긋난다.
        let hov = sb_cursor.0 >= mx
            && sb_cursor.0 <= mx + mw
            && sb_cursor.1 >= my
            && sb_cursor.1 <= my + mh;
        g.hover_pointer |= hov;
        // 색은 「누가 있나」를 한눈에, 명단은 「누가 뭘 하나」를 정확히.
        // 계단에 글자가 안 들어가서 이름은 이쪽으로 뺐다(2026-08-24 지시).
        // 마우스를 못 움직이는 헤드리스 검증에서 이 팝업만은 찍을 길이
        // 없어, env 로 첫 덱 칸의 명단을 펴 둔다(검증 전용).
        static FORCE_TIP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let force_tip =
            *FORCE_TIP.get_or_init(|| std::env::var("KASATERM_AUTODECKTIP").is_ok());
        if (hov || (force_tip && deck_tip.is_none())) && info.tab_peeks.len() > 1 {
            deck_tip = Some((mx + mw + 6.0, my, info.tab_peeks.clone()));
        }
        // 손이 필요한 칸은 **칸째 숨쉰다**(2026-08-11 지시: "점 말고 칸이
        // 빛나게"). 모서리 점으로도 말해 봤는데 6px 짜리가 얼굴 옆에 붙으니
        // 칸이 작을수록 얼룩처럼 읽혔고, 좁은 칸에서는 아예 그려지지도
        // 않았다(`mw > 16` 게이트). 칸 전체는 크기와 무관하게 보인다.
        let signal = if info.waiting {
            Some((theme::attention(), 0.9))
        } else if info.alert {
            Some((theme::accent(), 1.6))
        } else {
            None
        };
        round_rect(
            g,
            mx,
            my,
            mw,
            mh,
            2.0,
            // 신호가 테두리를 가져간다 — 「내가 여기 있다」(cur)보다
            // 「나를 기다린다」가 급한 소식이다.
            if let Some((c, _)) = signal {
                c
            } else if cur {
                // 「여기」는 도는 칸에서도 멈춘 테두리가 말한다 — 움직이는 윤곽
                // (`activity_mark`)은 그 안쪽을 돌아 둘이 한 줄로 뭉치지 않는다.
                theme::accent()
            } else if hov {
                theme::surface_hover()
            } else {
                // 기기 칸은 테두리가 기기색을 문다 — 채움(22%)만으로는
                // 옆 칸과 안 갈리고, 진하게 채우면 얼굴이 묻힌다.
                pane_identity::minimap_border(
                    theme::panel_bg(),
                    info.device.as_deref(),
                    theme::with_alpha(theme::border(), 0x66),
                )
            },
        );
        // 활성 칸은 **테두리로만** 표시한다. 통으로 칠하면 pane 이 하나인
        // 방에서 카드 머리 아래가 통짜 accent 덩어리가 되어, 배치도가
        // 아니라 잘못 칠해진 자리로 읽힌다(실측).
        if (cur || signal.is_some() || info.device.is_some()) && mw > 5.0 && mh > 5.0 {
            round_rect(
                g,
                mx + 1.5,
                my + 1.5,
                mw - 3.0,
                mh - 3.0,
                1.5,
                // 고른 칸도 채움은 기기색 그대로다 — 테두리(accent)가
                // 「여기」를 말하고, 색은 「어느 기기」를 말한다.
                pane_identity::minimap_background(
                    theme::panel_bg(),
                    info.device.as_deref(),
                ),
            );
        }
        // 숨쉬는 건 안쪽 판이다. 테두리까지 같이 흐려지면 칸의 윤곽이
        // 주기마다 사라져 배치도가 통째로 일렁인다.
        if let Some((col, period)) = signal {
            if mw > 5.0 && mh > 5.0 {
                let mut c = col;
                c[3] = signal_alpha(info.quiet, period);
                round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5, c);
            }
        }
        // 칸이 **누구 자리인지** 말한다. 목록을 걷어낸 이상 얼굴이 여기
        // 없으면 사이드바 어디에도 학생이 없다(사용자 2026-08-11: "미니맵은
        // 학생뭔지 보여야해"). 도는 중이면 줄에서 그랬듯 걷는다.
        // 상자는 바닥 띠 자리를 비우고 잡는다 — 걷기와 띠를 함께 그리기로
        // 한 이상(아래 진행 바 참고), 안 비우면 세로로 갈린 칸에서 발밑에
        // 띠가 파고든다(칸 31 · 얼굴 23 이면 2px).
        let (fx, fy, face) = minimap_face_box(mx, my, mw, mh);
        if info.error {
            let size = face.min(18.0);
            g.queue_icon(
                "triangle-alert",
                fx + (face - size) / 2.0,
                fy + (face - size) / 2.0,
                size,
                theme::danger(),
            );
        } else {
            let walked = info.busy
                && draw_student_walk(
                    g,
                    &info.who,
                    fx - 2.0,
                    fy - 2.0,
                    face + 4.0,
                    anim_phase_secs(),
                );
            if !walked
                && !draw_student_face_anim(
                    g,
                    &info.who,
                    fx,
                    fy,
                    face,
                    anim_phase_secs(),
                )
            {
                // 학생이 없는 자리 — 빈 칸으로 두면 "여긴 뭐지"가 되므로
                // 그 칸이 무엇인지 말해 둔다(웹=globe · 이미지 · md · 터미널).
                // 테마 없이 claude·codex 가 돌면 그 로고(칸마다 다른 색).
                let isz = face.min(16.0);
                let (ix, iy) = (mx + (mw - isz) / 2.0, my + (mh - isz) / 2.0);
                if !crate::sprites::draw_harness_logo(g, &info.agent, &info.pane, ix, iy, isz) {
                    g.queue_icon(info.icon, ix, iy, isz, theme::text_dim());
                }
            }
        }
        // 도는 칸은 **걷기와 함께** 칸 윤곽이 숨쉰다(사용자 2026-08-24 「미니맵에서
        // 진행중이면 걷기나 프로세스바가 아니라 둘다 나오게」 → 2026-10-07 바를 숨으로).
        // 바닥 띠 자리는 compact 눈금과 경과 시간이 쓴다.
        //
        // 처음 넣을 때는(2026-08-20) 걷는 칸에서 바를 뺐다 — 같은 뜻을 두
        // 겹으로 칠했다 되돌린 기록이 이 파일에 두 군데 있어서다(줄 vs 카드
        // 머리 · 칸 vs 머리 점). 그 판단이 뒤집혔고, 되풀이가 아닌 이유는
        // 둘이 닿는 거리가 다르기 때문이다: 걷기는 그 칸을 **들여다볼 때**
        // 읽히고, 칸 폭을 다 쓰는 띠는 **곁눈으로도** 잡힌다. 얼굴이 10px
        // 까지 작아지는 칸에서 걷는 다리는 사실상 안 보인다.
        //
        // 순서·대기 중 제외·모양은 `progress_bar` 가 한 곳에서 정한다 — 목록
        // 줄도 같은 띠를 쓴다(2026-10-01 「프로세스바 미니맵처럼」).
        if minimap_has_bar(mw, mh) {
            let (bx, by, mut bw) = (mx + 2.0, my + mh - MINI_BAR_H - MINI_BAR_PAD, mw - 4.0);
            // 경과 시간 — 바 오른쪽 끝을 내주고 바가 그만큼 짧아진다
            // (사용자 2026-08-24: 도는 것끼리 오래된 순서가 안 보인다).
            // 걷기·쓸림바는 「도는 중」만 말하지 「얼마나째」는 못 말하고,
            // 그건 compact 바에 % 를 붙인 것과 같은 종류의 부족함이다.
            //
            // **바를 밀어내지 않고 얹을 자리가 있을 때만** 그린다. 두 가지
            // 를 함께 본다 — ①바가 최소 10px 는 남아야 띠로 읽힌다(그 아래
            // 로는 점 두 개가 되어 무슨 표시인지 알 수 없다) ②글자가 얼굴
            // 오른쪽 밖에서 시작해야 한다. 칸은 5px 까지 작아지므로 좁은
            // 칸에서는 조용히 생략된다 — 거기서는 걷기와 바가 이미 「돈다」를
            // 말하고 있고, 시간까지 우겨넣으면 얼굴 위에 숫자가 겹친다.
            if let Some((txt, col, bold)) = elapsed_mark(info.busy_secs, info.busy || info.bg_active) {
                let fs = 9.0;
                let lw = g.measure_chrome_text(&txt, fs, bold);
                let lx = bx + bw - lw;
                if bw - lw - 3.0 >= 10.0 && lx >= fx + face + 2.0 {
                    g.draw_text(
                        lx,
                        // 바 위에 앉힌다 — 바와 세로 중앙을 맞추면 글자
                        // 아래가 칸 밖으로 나간다(바가 이미 바닥에서 2px).
                        by - fs + 2.0,
                        &txt,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: col,
                            bold,
                            italic: false,
                        },
                    );
                    bw -= lw + 3.0;
                }
            }
            progress_bar(g, bx, by, bw, info.compact_pct);
        }
        if signal.is_none() {
            activity_mark(g, (mx, my, mw, mh), 2.0, info.busy, info.bg_active, cur, if cur { 1.5 } else { 0.0 });
        }
        // 탭이 여럿인 pane 은 칸 바닥 왼쪽에 **점 줄** — 몇째 탭이 앞에
        // 나와 있는지. 전엔 뒷장이 우상단으로 계단지는 카드 덱이었는데,
        // 폰 배치도가 점으로 말하게 되면서 데스크톱도 같은 말로 맞췄다
        // (2026-09-08 지시 「점 표시 있으니까 겹침은 빼고 pc 도 모바일처럼」).
        // 명단은 그대로 마우스를 올리면 편다(`deck_tip`).
        let n_tabs = info.tab_peeks.len();
        if n_tabs > 1 && mw > 16.0 && mh > 16.0 {
            let (dot, gap) = (2.5, 1.5);
            let dy = if minimap_has_bar(mw, mh) {
                my + mh - MINI_BAR_H - MINI_BAR_PAD - dot - 2.0
            } else {
                my + mh - dot - 2.0
            };
            let shown = n_tabs.min(6);
            let mut dx = mx + 3.0;
            for t in info.tab_peeks.iter().take(shown) {
                let w = if t.active { dot * 1.8 } else { dot };
                let col = if t.active {
                    theme::text_dim()
                } else {
                    theme::with_alpha(theme::text_mute(), 0x70)
                };
                round_rect(g, dx, dy, w, dot, dot / 2.0, col);
                dx += w + gap;
            }
        }
        // 끌고 있는 칸이 떨어질 자리 — 대상 칸의 그 모서리에 accent 띠.
        // 목록 줄의 위/아래 선과 같은 말을 네 방향으로 한다.
        if let Some((tid, zone, src)) = sb_row_drop.as_ref() {
            if id == src {
                g.rect(mx, my, mw, mh, theme::with_alpha(theme::bg(), 0x88));
            }
            if id == tid {
                let bar = 3.0_f32.min(mw / 3.0).min(mh / 3.0).max(1.0);
                let (x, y, w, h) = match zone {
                    crate::DropZone::Left => (mx, my, bar, mh),
                    crate::DropZone::Right => (mx + mw - bar, my, bar, mh),
                    crate::DropZone::Up => (mx, my, mw, bar),
                    crate::DropZone::Down | crate::DropZone::Center => {
                        (mx, my + mh - bar, mw, bar)
                    }
                };
                g.rect(x, y, w, h, theme::accent());
            }
        }
    }
    // 「별도창」 띠 — 별도 OS 창으로 뗀 pane 이 떠나온 방 아래에 점선 칸으로
    // 앉는다. 얼굴·진행 바는 배치도 칸과 같은 말을 하고, 점선과 모서리의
    // 나가기 아이콘이 「지금 다른 창에 있다」를 말한다.
    let mut undock_tip: Option<(f32, f32, String)> = None;
    for ((_, id, r), info) in sb_undock.iter().zip(sb_undock_info.iter()) {
        let (mx, my, mw, mh) = *r;
        let hov = sb_cursor.0 >= mx
            && sb_cursor.0 <= mx + mw
            && sb_cursor.1 >= my
            && sb_cursor.1 <= my + mh;
        g.hover_pointer |= hov;
        if hov {
            let who = if info.who.is_empty() { id.as_str() } else { info.who.as_str() };
            undock_tip = Some((mx + mw + 6.0, my, format!("별도창 · {who}")));
        }
        let signal = if info.waiting {
            Some((theme::attention(), 0.9))
        } else if info.alert {
            Some((theme::accent(), 1.6))
        } else {
            None
        };
        round_rect(
            g,
            mx,
            my,
            mw,
            mh,
            2.0,
            pane_identity::minimap_background(if hov {
                theme::surface_hover()
            } else {
                theme::with_alpha(theme::surface(), 0x80)
            }, info.device.as_deref()),
        );
        if let Some((col, period)) = signal {
            if mw > 5.0 && mh > 5.0 {
                let mut c = col;
                c[3] = signal_alpha(info.quiet, period);
                round_rect(g, mx + 1.5, my + 1.5, mw - 3.0, mh - 3.0, 1.5, c);
            }
        }
        dashed_rect(
            g,
            mx,
            my,
            mw,
            mh,
            match signal {
                Some((c, _)) => c,
                None => theme::with_alpha(theme::border(), 0xaa),
            },
        );
        let (fx, fy, face) = minimap_face_box(mx, my, mw, mh);
        if info.error {
            let size = face.min(18.0);
            g.queue_icon(
                "triangle-alert",
                fx + (face - size) / 2.0,
                fy + (face - size) / 2.0,
                size,
                theme::danger(),
            );
        } else {
            let walked = info.busy
                && draw_student_walk(
                    g,
                    &info.who,
                    fx - 2.0,
                    fy - 2.0,
                    face + 4.0,
                    anim_phase_secs(),
                );
            if !walked
                && !draw_student_face_anim(g, &info.who, fx, fy, face, anim_phase_secs())
            {
                let isz = face.min(16.0);
                let (ix, iy) = (mx + (mw - isz) / 2.0, my + (mh - isz) / 2.0);
                if !crate::sprites::draw_harness_logo(g, &info.agent, &info.pane, ix, iy, isz) {
                    g.queue_icon(info.icon, ix, iy, isz, theme::text_dim());
                }
            }
        }
        if minimap_has_bar(mw, mh) {
            let by = my + mh - MINI_BAR_H - MINI_BAR_PAD;
            progress_bar(g, mx + 2.0, by, mw - 4.0, info.compact_pct);
        }
        if signal.is_none() {
            activity_mark(g, (mx, my, mw, mh), 2.0, info.busy, info.bg_active, false, 0.0);
        }
        if mw >= 22.0 {
            g.queue_icon("external-link", mx + mw - 10.0, my + 2.0, 8.0, theme::text_mute());
        }
    }
    g.pop_clip();
    (undock_tip, deck_tip)
}

/// 펼친 방의 pane 줄. 탭 카드가 그 자리를 이미 비워 뒀으므로(레이아웃이
/// 카드 높이에 목록만큼을 더해 준다) 여기서는 채우기만 한다.
pub(super) fn paint_rows(g: &mut gpu::GpuRenderer, s: &Strip, p: &Panes<'_>) {
    let sb_cursor = s.cursor;
    let Panes { rows: sb_rows, row_info: sb_row_info, row_drop: sb_row_drop, .. } = *p;
    for (k, ((wi, _, r), info)) in sb_rows.iter().zip(sb_row_info.iter()).enumerate() {
        let (who, label, col, is_cur) =
            (&info.who, &info.label, &info.color, info.is_cur);
        let (rx, ry, rw, rh) = *r;
        // 목록 보기의 두 줄 행 — 다른 기기 방과 같은 그림(`sidebar_pulse::paint_row`).
        // 아래 한 줄짜리 그림은 숨긴 pane 꼬리 줄만 쓴다.
        if !info.stashed && rh >= crate::sidebar_pulse::LIST_ROW_H {
            let hover = sb_cursor.0 >= rx && sb_cursor.0 <= rx + rw
                && sb_cursor.1 >= ry && sb_cursor.1 <= ry + rh;
            crate::sidebar_pulse::paint_row(g, *r, &crate::sidebar_pulse::RowPaint {
                student: &info.student,
                icon: info.icon,
                cur: is_cur,
                hover,
                busy: info.busy,
                compact_pct: info.compact_pct,
                bg_active: info.bg_active,
                busy_secs: info.busy_secs,
                muted: false,
            });
            continue;
        }
        // 줄 사이 실선 — 같은 방 안의 칸막이라 방과 방을 가르는
        // 카드 테두리보다 옅어야 한다. 첫 줄 위에는 안 긋는다(카드
        // 머리와 목록은 이미 여백으로 갈려 있다).
        if k > 0
            && sb_rows
                .get(k - 1)
                .map(|(pw, _, _)| pw == wi)
                .unwrap_or(false)
        {
            g.rect(
                rx + 6.0,
                ry,
                rw - 12.0,
                1.0,
                theme::with_alpha(theme::border(), 0x50),
            );
        }
        let row_hover = sb_cursor.0 >= rx
            && sb_cursor.0 <= rx + rw
            && sb_cursor.1 >= ry
            && sb_cursor.1 <= ry + rh;
        if row_hover {
            round_rect(
                g,
                rx,
                ry,
                rw,
                rh,
                theme::radius_sm(),
                theme::surface_hover(),
            );
        }
        // 지금 보고 있는 pane 은 왼쪽 띠로 — 목록이 방을 넘나들어서
        // 표시가 없으면 "내가 있는 곳"을 매번 번호로 대조하게 된다.
        if is_cur {
            g.rect(rx, ry + 3.0, 2.0, rh - 6.0, theme::accent());
        }
        // 도는 중이면 학생이 **걷는다**(2026-08-11 지시: "진행중인거 학생
        // 워크로 나오게하고"). 얼굴 GIF 는 도는 동안에도 가만히 앉아 있어
        // 줄만 봐서는 이 pane 이 일하는지 멈춰 있는지 알 수 없었고, 그걸
        // 대신 말하던 게 카드의 작업 점이었다 — 걷게 하면 그 점이 필요
        // 없어진다. 원본이 256 정사각(전신 + 여백)이라 상자도 정사각이다.
        // 얼굴보다 4px 키우는 건 그 여백 때문 — 같은 크기로 두면 캐릭터가
        // 얼굴 GIF 보다 작아 보여 걷기 시작할 때 줄이 움찔한다.
        let face = rh - 6.0;
        let walked = info.busy
            && draw_student_walk(
                g,
                who,
                rx + 5.0,
                ry + 1.0,
                rh - 2.0,
                anim_phase_secs(),
            );
        let has_face = walked
            || draw_student_face_anim(
                g,
                who,
                rx + 7.0,
                ry + 3.0,
                face,
                anim_phase_secs(),
            );
        let has_face = has_face
            || crate::sprites::draw_harness_logo(g, &info.agent, &info.pane, rx + 7.0, ry + 3.0, face);
        if !has_face {
            if info.icon != "terminal" {
                // 웹·이미지·md pane 줄 — 상태 점 대신 종류 아이콘.
                // 상태는 줄 끝 점이 이미 말한다.
                g.queue_icon(
                    info.icon,
                    rx + 7.0,
                    ry + (rh - 12.0) / 2.0,
                    12.0,
                    theme::text_dim(),
                );
            } else {
                circle_rect(g, rx + 9.0, ry + rh / 2.0 - 3.0, 6.0, *col);
            }
        }
        let name_x = rx + 7.0 + face + 6.0;
        // 경과 시간 — 상태 점 왼쪽. 이 줄에 오는 건 이제 **숨긴 pane**
        // 뿐이다(목록 뷰가 없어졌다). 숨긴 pane 은 트리에 없어 배치도
        // 칸이 아예 없으므로, 여기서 안 말하면 화면 어디에도 없다.
        let elapsed = info
            .busy_secs
            .filter(|_| info.busy || info.bg_active)
            .and_then(elapsed_label)
            .map(|t| {
                let (col, bold) = elapsed_style(info.busy_secs.unwrap_or(0));
                (t, col, bold)
            });
        // 이름 예산을 시간만큼 내준다 — 안 빼면 긴 제목이 시간 위로
        // 그려져 두 글자가 겹친 채 읽힌다(`clip_px` 는 자기 예산만 안다).
        let elapsed_w = elapsed
            .as_ref()
            .map(|(t, _, b)| g.measure_chrome_text(t, 10.0, *b) + 5.0)
            .unwrap_or(0.0);
        if let Some((t, col, bold)) = &elapsed {
            g.draw_text(
                rx + rw - 14.0 - (elapsed_w - 5.0),
                ry + (rh - 10.0) / 2.0,
                t,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: *col,
                    bold: *bold,
                    italic: false,
                },
            );
        }
        // 원격 pane 은 기계 이름 칩을 이름 앞에 — 얼굴·이름이 로컬과
        // 똑같아서, 이게 없으면 목록만 봐서는 어느 기계에서 도는지 알 수
        // 없다. 이름 예산에서 칩 폭을 미리 뺀다(겹침 방지, elapsed 와 같다).
        let mut name_x = name_x;
        if let Some(m) = info.machine.as_deref() {
            let chip_f = 9.0_f32;
            let cw = g.measure_chrome_text(m, chip_f, false) + 8.0;
            let ch = 13.0_f32;
            let cy = ry + (rh - ch) / 2.0;
            let fill = theme::raised_on(theme::surface(), false);
            crate::panel_rect_outlined(g, name_x, cy, cw, ch, theme::radius_sm(), fill);
            g.draw_text(
                name_x + 4.0,
                cy + (ch - chip_f) / 2.0,
                m,
                gpu::DrawOpts {
                    font_size: chip_f,
                    color: theme::accent(),
                    bold: true,
                    italic: false,
                },
            );
            name_x += cw + 5.0;
        }
        let budget = (rx + rw - 14.0 - elapsed_w - name_x).max(0.0);
        let txt = clip_px(g, label, 11.0, false, budget);
        g.draw_text(
            name_x,
            ry + (rh - 11.0) / 2.0,
            &txt,
            gpu::DrawOpts {
                font_size: 11.0,
                // 숨긴 줄은 한 단 더 낮춘다 — 목록에 남아 있되 「지금 화면에
                // 있는 것」과 한눈에 갈려야 한다.
                color: if info.stashed {
                    theme::text_mute()
                } else if is_cur {
                    theme::text()
                } else {
                    theme::text_dim()
                },
                bold: false,
                italic: false,
            },
        );
        // 줄 끝 상태 점. 손이 필요한 줄에서는 **이 점이 깜빡인다** —
        // 예전엔 줄 전체를 숨쉬게 해서 알렸는데, 넓은 판이 은근히 밝아지는
        // 신호는 22px 줄에서는 배경 얼룩처럼 읽혔고 두 줄이 동시에 서면
        // 목록이 통째로 일렁였다(2026-08-11 지시: "숨쉬기말고 동그라미
        // 깜빡이게"). 점은 이미 그 줄의 상태를 말하던 자리라, 새 표시를
        // 더하는 대신 있던 것을 깜빡이게 하면 목록에 늘어나는 게 없다.
        let dot_x = rx + rw - 6.0;
        let dot_y = ry + rh / 2.0 - 3.0;
        if info.stashed {
            // 숨김 표시가 상태 점 자리를 대신 쓴다. 치워 둔 줄에 상태 점을
            // 그대로 두면 화면에 있는 줄과 구분이 안 된다 — 그리고 어차피
            // 그 상태(도는 중·기다림)는 화면에 없는 pane 의 것이라 지금
            // 손댈 수 있는 신호가 아니다.
            // ⚠️ 이름은 `icon_svg`(gpu.rs)에 **등록된 것**이어야 한다 — 없는
            // 이름은 오류 없이 아무것도 안 그린다(실측: "disabled" 로 두어
            // 표시가 통째로 사라졌고, 흐린 글자만 남아 원인이 안 보였다).
            g.queue_icon("minus", dot_x - 1.5, dot_y - 1.5, 9.0, theme::text_mute());
        } else if info.waiting && info.quiet {
            circle_rect(g, dot_x, dot_y, 6.0, theme::attention());
        } else if info.waiting {
            blink_dot(g, dot_x, dot_y, 6.0, theme::attention(), 0.9);
        } else if info.alert && info.quiet {
            circle_rect(g, dot_x, dot_y, 6.0, theme::accent());
        } else if info.alert {
            blink_dot(g, dot_x, dot_y, 6.0, theme::accent(), 1.6);
        } else {
            circle_rect(g, dot_x, dot_y, 6.0, *col);
        }
    }
    // 끌고 있는 줄이 떨어질 자리 — 대상 줄의 위/아래 모서리에 긋는다.
    // 끌리는 줄 자신은 옅게 낮춰 "지금 손에 들려 있다"를 남긴다.
    if let Some((tid, zone, src)) = sb_row_drop.as_ref() {
        for (_, id, r) in sb_rows.iter() {
            if id == src {
                g.rect(r.0, r.1, r.2, r.3, theme::with_alpha(theme::bg(), 0x88));
            }
            if id == tid {
                let ly = if *zone == crate::DropZone::Up {
                    r.1
                } else {
                    r.1 + r.3 - 2.0
                };
                g.rect(r.0 + 4.0, ly, r.2 - 8.0, 2.0, theme::accent());
            }
        }
    }
}

pub(super) fn paint_edges(g: &mut gpu::GpuRenderer, s: &Strip, e: &Edges<'_>) {
    let Strip { w: tab_strip_w, view: sb_view, cursor: sb_cursor } = *s;
    let Edges {
        tabs: sb_tabs,
        plus: sb_plus,
        over_before: sb_over_before,
        over_after: sb_over_after,
        scroll: sb_scroll,
        drag_target: win_drag_target,
        menu_open,
        tray: sidebar_tray,
    } = *e;
    // Device creation lives below the shared device list; each device header owns room creation.
    let (px, py, pw, ph) = sb_plus;
    let plus_hover = sb_cursor.0 >= px
        && sb_cursor.0 <= px + pw
        && sb_cursor.1 >= py
        && sb_cursor.1 <= py + ph;
    if plus_hover {
        hover_rect(g, px, py, pw, ph, theme::radius_md());
    }
    let label = "기기 추가";
    let label_w = g.measure_chrome_text(label, 11.5, false);
    let show_label = pw >= label_w + theme::ICON_SIZE + 24.0;
    let total_w = theme::ICON_SIZE + if show_label { 8.0 + label_w } else { 0.0 };
    let left = px + (pw - total_w) / 2.0;
    g.queue_icon("monitor", left, py + (ph - theme::ICON_SIZE) / 2.0,
        theme::ICON_SIZE, if plus_hover { theme::text() } else { theme::text_dim() });
    g.queue_icon("plus", left + theme::ICON_SIZE - 4.0, py + ph / 2.0 + 1.0,
        8.0, if plus_hover { theme::text() } else { theme::text_dim() });
    if show_label {
        g.draw_text(left + theme::ICON_SIZE + 8.0, py + (ph - 11.5) / 2.0 - 1.0,
            label, gpu::DrawOpts { font_size: 11.5, color: theme::text_dim(), bold: false, italic: false });
    }
    // Overflow chevrons: up in the slot above the first tab, down
    // under the "+" — more windows exist past that edge, wheel
    // over the strip scrolls the run.
    if let Some((_, (ftx, _, ftw, _))) = sb_tabs.first() {
        let cis = 12.0_f32;
        let ccx = ftx + (ftw - cis) / 2.0;
        if sb_over_before {
            g.queue_icon(
                "chevron-up",
                ccx,
                sb_view.0 - 13.0,
                cis,
                theme::text_mute(),
            );
        }
        if sb_over_after {
            g.queue_icon("chevron-down", ccx, py + ph + 4.0, cis, theme::text_mute());
        }
    }
    // 목록이 넘칠 때의 가장자리 표시 — 파일트리 칼럼과 같은 모양이다
    // (흐림 + 올렸을 때만 뜨는 막대). chevron 은 남긴다: 막대가 hover
    // 전용이라, 손을 안 올린 평소엔 저 화살표가 유일한 힌트다.
    if let Some((view_top, viewport_h, content_h, scrolled)) = sb_scroll {
        let view_bottom = view_top + viewport_h;
        let overflow = content_h - viewport_h;
        let fade_h = 28.0_f32;
        let strips = 16;
        let strip_h = fade_h / strips as f32 + 0.5;
        // 위쪽 흐림은 첫 픽셀에 탁 켜지지 않게 `fade_h` 만큼 스크롤하는
        // 동안 서서히 들어온다.
        if scrolled > 0.5 {
            let k = (scrolled / fade_h).min(1.0);
            for i in 0..strips {
                let t = i as f32 / (strips - 1) as f32;
                let a = ((1.0 - t) * 0.92 * k * 255.0) as u8;
                g.rect(
                    0.0,
                    view_top + t * fade_h,
                    tab_strip_w - 1.0,
                    strip_h,
                    theme::with_alpha(theme::panel_bg(), a),
                );
            }
        }
        if scrolled < overflow - 0.5 {
            let k = ((overflow - scrolled) / fade_h).min(1.0);
            for i in 0..strips {
                let t = i as f32 / (strips - 1) as f32;
                let a = (t * 0.92 * k * 255.0) as u8;
                g.rect(
                    0.0,
                    view_bottom - fade_h + t * fade_h,
                    tab_strip_w - 1.0,
                    strip_h,
                    theme::with_alpha(theme::panel_bg(), a),
                );
            }
        }
        let over_col = sb_cursor.0 >= 0.0
            && sb_cursor.0 < tab_strip_w
            && sb_cursor.1 >= view_top
            && sb_cursor.1 < view_bottom;
        if over_col {
            let thumb_h = (viewport_h * viewport_h / content_h).max(28.0);
            let thumb_y = view_top
                + (viewport_h - thumb_h) * (scrolled / overflow).clamp(0.0, 1.0);
            pill_rect(
                g,
                tab_strip_w - 6.0,
                thumb_y,
                3.5,
                thumb_h,
                theme::with_alpha(theme::text(), 0x66),
            );
        }
    }
    // 재배치 드래그 중이면 떨어질 자리에 가로 막대. 마지막 탭 아래로
    // 미는 경우만 끝 모서리에 붙는다(target == 마지막 + 1).
    if let Some(t) = win_drag_target {
        let bar_y = sb_tabs
            .iter()
            .find(|(i, _)| *i == t)
            .map(|(_, r)| r.1 - SIDEBAR_TAB_GAP / 2.0)
            .or_else(|| {
                sb_tabs
                    .last()
                    .filter(|(i, _)| t == i + 1)
                    .map(|(_, r)| r.1 + r.3 + SIDEBAR_TAB_GAP / 2.0)
            });
        if let (Some(by), Some((_, fr))) = (bar_y, sb_tabs.first()) {
            g.rect(fr.0, by - 1.5, fr.2, 3.0, theme::accent());
        }
    }
    // ── 하단 트레이 ── 기기 추가. 목록과 얇은 선으로
    // 갈라 "목록의 마지막 항목"이 아니라 별도 층으로 읽히게 한다.
    // "+" 피커가 열려 있으면 스킵 — 팝업이 이 자리를 덮는데 아이콘
    // 글리프는 rect 위 레이어라 비쳐 올라온다(가려지는 chrome 은 안
    // 그린다는 관례).
    if !menu_open {
        if let Some(tray) = sidebar_tray {
            let line_y = tray.line_y;
            g.rect(
                SIDEBAR_TAB_INSET,
                line_y,
                (tab_strip_w - SIDEBAR_TAB_INSET * 2.0).max(0.0),
                1.0,
                theme::border(),
            );
        }
    }
}

pub(super) fn paint_row_menu(
    g: &mut gpu::GpuRenderer,
    s: &Strip,
    sb_win_h: f32,
    m: &RowMenu<'_>,
    rects: &mut Vec<(SidebarMenuAction, Rect)>,
) {
    let Strip { w: tab_strip_w, cursor: sb_cursor, .. } = *s;
    let ((mx0, my0), pane) = (m.anchor, m.pane);
    let pulse_menu_label = m.pulse_label;
    // 이미 숨긴 줄이면 되돌리기 한 갈래만 낸다 — 같은 자리에서 같은
    // 동작을 토글로 부르는 편이 항목 두 개를 늘 보여주는 것보다 낫다.
    let hidden = m.hidden;
    // pane 이 비면 방 메뉴 — 본문 보기 전환·이름·닫기(2026-09-08 지시).
    let items: Vec<(SidebarMenuAction, &str)> = if pane.is_empty() {
        vec![
            // 문구는 **그 방** 기준이다 — 옆 방이 목록이라고 이 방
            // 메뉴가 「배치도로 보기」로 뜨면 누를 때마다 어긋난다.
            if m.room_is_list {
                (SidebarMenuAction::MapBody, "배치도로 보기")
            } else {
                (SidebarMenuAction::ListBody, "목록으로 보기")
            },
            (SidebarMenuAction::RenameRoom, "이름 바꾸기"),
            (SidebarMenuAction::CloseRoom, "방 닫기"),
            (SidebarMenuAction::TogglePulse, pulse_menu_label),
        ]
    } else if hidden {
        // 숨긴 것은 「무엇이었나」부터 궁금하다 — 배치를 안 건드리고 보는
        // 길을 먼저 둔다(2026-09-21 지시 「숨긴건 누르면 미리보기랑 보이기」).
        vec![(SidebarMenuAction::Unhide, "다시 보이기")]
    } else if m.undocked {
        // 별도창 pane 에 「숨기기」를 주면 stash 가 remove_pane 으로 새어
        // 트리 밖 pane 을 죽인다 — 되돌리기 하나만.
        vec![(SidebarMenuAction::Dock, "본창으로 되돌리기")]
    } else {
        // 다른 기기 pane 메뉴와 같은 두 줄이다 — 자리가 같으면 항목도 같아야
        // 누른 사람이 기계마다 다른 것을 외우지 않는다.
        let mut v = vec![
            (SidebarMenuAction::ClosePane, "pane 닫기"),
            (SidebarMenuAction::Hide, "pane 숨기기"),
        ];
        if m.weather_on {
            v.extend(crate::weather::model::PaneWeather::MENU.iter().map(|&w| (SidebarMenuAction::Weather(w), w.menu_label())));
        }
        v
    };
    let weather_now = m.weather_now;
    const MIH: f32 = 28.0;
    let widest = items
        .iter()
        .map(|(_, l)| g.measure_chrome_text(l, 13.0, false))
        .fold(0.0f32, f32::max);
    let mw = (widest + 32.0).min((tab_strip_w - 8.0).max(80.0));
    let mh = 12.0 + items.len() as f32 * MIH;
    // 사이드바 안에 가둔다 — 넘치면 오른쪽 파일트리 위로 삐져나간다.
    // 시저로 자를 수도 있지만, 반쯤 잘린 메뉴는 읽을 수가 없다.
    // 안 보이게 자르는 것보다 자리를 옮겨 다 보이는 편이 낫다.
    let mx = mx0.min((tab_strip_w - mw - 4.0).max(4.0)).max(4.0);
    let my = my0
        .min((sb_win_h - mh - 6.0).max(TITLE_HEIGHT))
        .max(TITLE_HEIGHT);
    panel_rect_outlined(g, mx, my, mw, mh, theme::radius_md(), theme::surface());
    rects.clear();
    for (i, (a, label)) in items.iter().enumerate() {
        let r = (mx + 4.0, my + 6.0 + i as f32 * MIH, mw - 8.0, MIH);
        let hov = sb_cursor.0 >= r.0
            && sb_cursor.0 <= r.0 + r.2
            && sb_cursor.1 >= r.1
            && sb_cursor.1 <= r.1 + r.3;
        g.hover_pointer |= hov;
        if hov {
            hover_rect(g, r.0, r.1, r.2, r.3, theme::radius_sm());
        }
        let chosen = *a == SidebarMenuAction::Weather(weather_now);
        g.draw_text(
            r.0 + 12.0,
            r.1 + (MIH - 13.0) / 2.0,
            label,
            gpu::DrawOpts {
                font_size: 13.0,
                color: if chosen { theme::accent() } else { theme::text() },
                bold: chosen,
                italic: false,
            },
        );
        rects.push((*a, r));
    }
}
