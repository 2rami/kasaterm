//! 사이드바 방 카드 — 카드 높이·스크롤 범위·배치 계산.
use super::*;

impl App {
    /// 방 카드 한 장의 치수 — `(body_h, list_h, hidden, undocked)`. 카드 전체 높이는
    /// `SIDEBAR_TAB_H + list_h` 이고, 접힌 방은 `list_h == 0` 이다.
    ///
    /// 배치 루프와 스크롤 한계가 **같은 값**을 봐야 한다. 갈라지면 스크롤은 되는데
    /// 목록 끝이 영영 안 나오는 종류로 어긋난다 — 실제로 그랬다(아래 참조).
    pub(super) fn sidebar_card_metrics(&self, i: usize) -> (f32, f32, Vec<String>, Vec<String>) {
        // 설정·보드 카드는 펴지 않는다. 배치도는 「어느 학생이 어디 있나」를 그리는
        // 자리인데 내부 방의 leaf 는 화면 표식 하나뿐이라, 펴면 아이콘 한 칸짜리
        // 빈 지도가 남는다(2026-09-07 지적 「설정창 방미니맵 그건 필요없으니까」).
        // 배지가 없어 손으로는 못 펴지만, 방을 닫거나 옮기며 인덱스가 당겨지면
        // 옆 방의 펼침 상태를 물려받아 저절로 펴졌다.
        if self.internal_room_kind_at(i).is_some() {
            return (0.0, 0.0, Vec::new(), Vec::new());
        }
        let leaves = self.window_leaves(i);
        // 별도 OS 창으로 뗀 pane 도 트리에 없다 — 배치도 밑 띠에 칸으로 둔다.
        let undocked = self.room_undocked(i);
        // 숨긴 pane 은 트리에 없어 배치도에 칸이 없다 — 지도 아래 꼬리 줄로 둔다.
        // 어디에도 안 보이면 되살릴 길이 없고, 트리에서 빠졌을 뿐 PTY 는 돈다.
        let hidden: Vec<String> = self
            .closed_panes
            .iter()
            .filter(|c| c.stashed && c.alive && c.window == i)
            .map(|c| c.pane_id.clone())
            .collect();
        // 학생이 하나인 방도 편다. "점 하나가 이미 그 하나를 말한다"고 봤는데,
        // 그 한 줄이 **누가 있고 무슨 상태인지의 전부**라 접어 두면 학생 하나짜리
        // 방에선 그 학생을 볼 길이 통째로 사라졌다(2026-08 지적, 두 번).
        // 펴는 중이면 0..1 사이 — 카드가 그만큼만 자란다.
        let t = if leaves.is_empty() && undocked.is_empty() {
            0.0
        } else {
            self.expand_progress(i)
        };
        // 칸이 얼굴을 담아야 하므로 높이가 pane 수를 따라간다 — 여섯 칸을 46px
        // 안에 우겨넣으면 한 칸이 7px 이라 얼굴이 안 들어간다.
        // 목록 보기면 본문은 학생 줄이 pane 수만큼 — 배치도 대신이다(2026-09-08 지시).
        // 목록 줄은 두 줄(40)이고 거울 줄을 뺀다 — 원본 기기 줄과 같은 학생이다.
        let body_h = if self.room_body_is_list(i) {
            self.sidebar_list_panes(i).len() as f32 * crate::sidebar_pulse::LIST_ROW_H + crate::sidebar_pulse::LIST_TOP_GAP
        } else {
            (36.0 + 13.0 * leaves.len() as f32).clamp(46.0, 150.0)
        };
        let strip_h = if undocked.is_empty() { 0.0 } else { UNDOCK_STRIP_H };
        let full_h = body_h + strip_h + hidden.len() as f32 * SIDEBAR_ROW_H + SIDEBAR_ROW_PAD;
        (body_h, (full_h * t).round(), hidden, undocked)
    }

    /// 방 카드 전체 높이 목록(갭 제외). 스크롤 한계·막대 위치·굴림 한 칸이
    /// **같은 값**을 봐야 하므로 한 곳에서만 만든다.
    pub(crate) fn sidebar_card_heights(&self) -> Vec<f32> {
        (0..self.windows.len())
            .map(|i| {
                // 다른 기기 방의 보기 창은 이 목록에 안 선다 — 높이 0.
                if self.remote_view_of_window(i).is_some() {
                    return 0.0;
                }
                SIDEBAR_TAB_H + self.sidebar_card_metrics(i).1
            })
            .collect()
    }

    /// 방 목록이 세로로 쓸 수 있는 높이(logical px). 트레이·독·상태줄이 바닥을
    /// 먹고, 24px 는 chevron-down 오버플로 힌트 자리다.
    pub(crate) fn sidebar_avail_h(&self, win_h: f32) -> f32 {
        self.sidebar_full_avail_h(win_h)
    }

    pub(crate) fn sidebar_local_content_h(&self) -> f32 {
        self.sidebar_card_heights().iter().filter(|h| **h > 0.0)
            .map(|h| h + SIDEBAR_TAB_GAP).sum()
    }

    pub(crate) fn sidebar_content_h(&self) -> f32 {
        self.sidebar_local_content_h() + crate::sidebar_navigation::remote_content_h(&self.info)
    }

    /// 아래 절까지 포함한 세로 구간 — 절 배치는 이 값으로 잰다.
    pub(crate) fn sidebar_full_avail_h(&self, win_h: f32) -> f32 {
        // 10px slot above the first tab hosts the overflow chevron-up.
        let top = self.sidebar_content_top();
        // 상태줄도 바닥을 먹는다. 안 빼면 마지막 방 카드가 그 위로 넘치는데,
        // 사이드바는 클립을 안 세우므로 **잘리지 않고 그대로 덮어 그려진다** — 화면은
        // 멀쩡해 보이고 카드만 엉뚱한 자리에 있는 종류의 버그가 된다.
        let bottom_h = self.status_h();
        (win_h - bottom_h - top - SIDEBAR_TRAY_H - 24.0).max(SIDEBAR_TAB_H + SIDEBAR_TAB_GAP)
    }

    /// 방 목록 스크롤 기하 — `(view_top, viewport_h, content_h, scrolled_px)`.
    /// 넘치지 않으면 `None`(그릴 이유가 없다).
    ///
    /// `win_tab_first` 는 **인덱스**라 그대로는 막대 위치가 안 된다. 카드 높이가
    /// 제각각이라 인덱스 비율과 픽셀 비율이 어긋나므로, 앞쪽 카드 높이를 실제로
    /// 더해 픽셀로 환산한다.
    pub(crate) fn sidebar_scroll_geom(&self, win_h: f32) -> Option<(f32, f32, f32, f32)> {
        if self.tabs_on_top {
            return None;
        }
        let n = self.windows.len();
        if n == 0 {
            return None;
        }
        let content_h = self.sidebar_content_h();
        let viewport_h = self.sidebar_avail_h(win_h);
        if content_h <= viewport_h + 0.5 {
            return None;
        }
        let scrolled = self
            .sidebar_scroll_px
            .clamp(0.0, (content_h - viewport_h).max(0.0));
        Some((self.sidebar_content_top(), viewport_h, content_h, scrolled))
    }

    /// 목록을 끝까지 내렸을 때의 스크롤 위치(px). 넘치지 않으면 0.
    pub(crate) fn sidebar_max_scroll(&self, win_h: f32) -> f32 {
        (self.sidebar_content_h() - self.sidebar_avail_h(win_h)).max(0.0)
    }

    pub(crate) fn sidebar_layout(
        &self,
        win_h: f32,
    ) -> (
        Vec<(usize, (f32, f32, f32, f32))>,
        Vec<(usize, (f32, f32, f32, f32))>,
        (f32, f32, f32, f32),
        Vec<(usize, String, (f32, f32, f32, f32))>,
        // 배치도 칸 — 목록 행과 **같은 모양**이라 히트 벡터에 그대로 합칠 수 있다.
        // 그러면 칸 클릭·드래그·우클릭이 행과 똑같이 동작한다(공짜로 따라온다).
        Vec<(usize, String, (f32, f32, f32, f32))>,
        // 「별도창」 띠 칸 — 모양은 같지만 히트 벡터엔 **안 합친다**. 행 경로는
        // 드래그·focus_pane 을 켜는데 트리 밖 pane 엔 둘 다 틀린 동작이다.
        Vec<(usize, String, (f32, f32, f32, f32))>,
    ) {
        let n = self.windows.len();
        if self.tabs_on_top {
            // Horizontal tabs in the title strip (Windows Terminal-style).
            // Same return shape as the vertical layout so the paint loop and
            // every click/drag hit-test keep working off the cached rects.
            let win_w = self
                .window
                .as_ref()
                .map(|w| w.inner_size().width as f32 / self.effective_scale())
                .unwrap_or(1200.0);
            let (tbx, _, tbw, _) = self.file_tree_toggle_rect();
            // 14px slots at both ends host the overflow chevrons — reserved
            // unconditionally so geometry doesn't depend on overflow state.
            let x0 = tbx + tbw + 10.0 + 14.0;
            // Right-side chip cluster (arona + settings + git-col) stays clear.
            let right_reserved = 110.0 + 14.0;
            let plus_w = 26.0;
            let gap = 4.0;
            let avail = (win_w - right_reserved - x0 - plus_w - 6.0).max(60.0);
            // Whole tabs that fit at the 72px minimum width; hidden rest is
            // reachable by wheel (win_tab_first) and stays out of the rects.
            let n_vis = n.min((((avail + gap) / (72.0 + gap)) as usize).max(1));
            let first = self.win_tab_first.min(n.saturating_sub(n_vis));
            let tab_w = ((avail - gap * n_vis.saturating_sub(1) as f32) / n_vis.max(1) as f32)
                .clamp(72.0, 170.0);
            let tab_h = 26.0;
            let y = (TITLE_HEIGHT - tab_h) / 2.0;
            let mut tabs = Vec::with_capacity(n_vis);
            let mut closes = Vec::new();
            for (vi, i) in (first..n.min(first + n_vis)).enumerate() {
                let x = x0 + vi as f32 * (tab_w + gap);
                tabs.push((i, (x, y, tab_w, tab_h)));
                if n > 1
                    && (self.internal_room_kind_at(i).is_some() || self.user_room_count() > 1)
                {
                    let cs = 14.0;
                    closes.push((i, (x + tab_w - cs - 5.0, y + (tab_h - cs) / 2.0, cs, cs)));
                }
            }
            let plus = (x0 + tabs.len() as f32 * (tab_w + gap), y, plus_w, tab_h);
            // 가로 탭엔 아래로 펼 자리가 없다 — pane 목록도 배치도도 세로 전용.
            return (tabs, closes, plus, Vec::new(), Vec::new(), Vec::new());
        }
        let tab_x = SIDEBAR_TAB_INSET;
        let tab_w = (self.tab_strip_w() - 2.0 * SIDEBAR_TAB_INSET).max(0.0);
        // 10px slot above the first tab hosts the overflow chevron-up.
        let top = self.sidebar_content_top();
        // Rows that fit above the "+" button; the status bar eats the bottom
        // of the column, and 24px stays free for "+"-adjacent chrome + the
        // chevron-down overflow hint.
        let avail_h = self.sidebar_avail_h(win_h);
        // 스크롤 한계는 **실제 카드 높이**로 잡는다. 접힌 높이로 칸을 나눠 세면
        // 펼친 방이 있을 때 한계가 0 으로 나와 목록 끝이 손에 안 닿는다.
        //
        // 방 × 를 연달아 누르는 동안엔 그 한계를 안 본다. 방이 줄면 한계도 같이
        // 줄어 목록이 아래로 당겨지는데, 그러면 다음 × 가 딴 자리로 간다.
        let scroll = match self
            .close_freeze
            .sidebar_scroll
            .filter(|_| self.close_freeze.live())
        {
            Some(px) => px,
            None => self
                .sidebar_scroll_px
                .clamp(0.0, self.sidebar_max_scroll(win_h)),
        };
        let mut tabs = Vec::new();
        let mut closes = Vec::new();
        let mut rows = Vec::new();
        let mut mini = Vec::new();
        let mut undock = Vec::new();
        // 펼친 방은 카드가 pane 수만큼 길어진다 — 고정 stride 를 쓰던 자리를 누적
        // y 로 바꾼 이유가 이것이다. 넘치는 방은 그리지 않는다(사이드바는 클립을
        // 안 세워서 반쪽 카드가 트레이를 침범한다).
        //
        // 여기는 배치 계산이라 시저를 세워도 이 규칙은 남는다 — 이 rect 들은 그리기와
        // 클릭 판정이 함께 쓰는 값이고, 클릭은 시저가 안 자른다.
        let mut y = top - scroll;
        for i in 0..n {
            // 다른 기기 방의 보기 창은 그 기계 절의 방 카드가 탭이다 — 여기엔 안 세운다.
            if self.remote_view_of_window(i).is_some() {
                continue;
            }
            // 펼친 카드는 **배치도 하나**다. 예전엔 목록 뷰로 갈아 끼울 수 있었는데,
            // 그 목록은 info 탭이 방→pane→탭→프로세스로 이미 그리는 것의 얕은
            // 사본이었다(2026-08-24 지시: "목록표시는 info에서 보면되고").
            // 치수는 `sidebar_card_metrics` 하나에서 나온다 — 스크롤 한계도 같은
            // 함수를 보므로 배치와 한계가 갈릴 수 없다.
            let (body_h, list_h, hidden, undocked) = self.sidebar_card_metrics(i);
            let h = SIDEBAR_TAB_H + list_h;
            // 뷰포트에 조금도 안 걸치면 rect 를 아예 안 낸다. **시저는 픽셀만 자르지
            // 클릭은 안 자르므로**, 밖에 있는 카드를 등록하면 화면엔 없는 방이
            // 눌린다. 걸친 카드는 그대로 낸다 — 잘라 그리는 건 시저 몫이고, 히트는
            // 렌더가 뷰포트와 교집합 내서 등록한다.
            if y + h <= top || y >= top + avail_h {
                y += h + SIDEBAR_TAB_GAP;
                continue;
            }
            tabs.push((i, (tab_x, y, tab_w, h)));
            if n > 1
                && (self.internal_room_kind_at(i).is_some() || self.user_room_count() > 1)
            {
                let cs = 14.0;
                // Centered on the *name* row (drawn at y+11, 13.5px) rather than
                // pinned to the card top — the two-line tab put the × above the
                // title it belongs to, reading as detached from both lines.
                closes.push((i, (tab_x + tab_w - cs - 3.0, y + 11.0, cs, cs)));
            }
            if list_h > 0.0 {
                // 카드 안에 온전히 들어온 줄만 낸다 — 사이드바는 클립을 안 세워서
                // 반쪽 줄이 카드 밖으로 삐져나온다. 그래서 목록이 아래에서 한 줄씩
                // 드러난다.
                let bottom = y + h;
                // 배치도 — 카드 머리 바로 아래. `leaf_rects` 가 BSP 트리를 사각형으로
                // 이미 풀어 주므로 여기서 재귀할 것이 없다.
                let ma = (
                    tab_x + 10.0,
                    y + SIDEBAR_TAB_H + 3.0,
                    tab_w - 20.0,
                    body_h - 8.0,
                );
                if self.room_body_is_list(i) {
                    // 목록 보기 — 배치도 자리에 학생 줄(내 차례 → 하는 중 → 쉬는 중). 숨긴 줄은 그 아래 이어진다.
                    let row_h = crate::sidebar_pulse::LIST_ROW_H;
                    for (k, id) in self.sidebar_list_panes(i).into_iter().enumerate() {
                        let ry = y + SIDEBAR_TAB_H + crate::sidebar_pulse::LIST_TOP_GAP + k as f32 * row_h;
                        if ry + row_h > bottom {
                            break;
                        }
                        rows.push((i, id, (tab_x + 8.0, ry, tab_w - 16.0, row_h)));
                    }
                } else if ma.1 + ma.3 <= bottom && ma.2 > 0.0 {
                    // 활성 방의 트리는 `windows[i]` 가 아니라 `pty_layout` 에 있다
                    // (그 슬롯은 None 이다) — `window_leaves` 와 같은 갈래를 쓴다.
                    let tree = if i == self.active_window {
                        self.pty_layout.as_ref()
                    } else {
                        self.windows.get(i).and_then(|o| o.as_ref())
                    };
                    // 1000 을 기준으로 뽑는다. 작은 값(예: 100)을 넣으면 u16 반올림에
                    // 얇은 pane 이 0 폭으로 뭉개진다.
                    const G: f32 = 1000.0;
                    let cells = tree.map(|t| t.leaf_rects(1000, 1000)).unwrap_or_default();
                    for (id, cx, cy, cw, ch) in cells {
                        // 1px 씩 깎아 칸 사이에 틈을 낸다 — 붙여 놓으면 분할선이 안 보여
                        // 한 덩어리로 읽힌다.
                        mini.push((
                            i,
                            id,
                            (
                                ma.0 + cx as f32 / G * ma.2,
                                ma.1 + cy as f32 / G * ma.3,
                                (cw as f32 / G * ma.2 - 1.0).max(2.0),
                                (ch as f32 / G * ma.3 - 1.0).max(2.0),
                            ),
                        ));
                    }
                }
                // 별도창 띠 — 배치도와 숨김 꼬리 사이. 칸은 방 폭을 pane 수로 나눈다.
                let strip_h = if undocked.is_empty() { 0.0 } else { UNDOCK_STRIP_H };
                let sy = y + SIDEBAR_TAB_H + body_h;
                if strip_h > 0.0 && sy + strip_h <= bottom {
                    let cnt = undocked.len() as f32;
                    let gap = 3.0;
                    let cw = ((tab_w - 20.0 - gap * (cnt - 1.0)) / cnt).clamp(2.0, 40.0);
                    for (k, id) in undocked.iter().enumerate() {
                        undock.push((
                            i,
                            id.clone(),
                            (tab_x + 10.0 + k as f32 * (cw + gap), sy + 2.0, cw, strip_h - 4.0),
                        ));
                    }
                }
                // 배치도 아래 꼬리에는 숨긴 pane 만 줄로 남는다 — 숨긴 것은 트리에
                // 없어 칸이 없으므로 배치도로는 말할 방법이 아예 없다.
                for (k, id) in hidden.iter().enumerate() {
                    let ry = y
                        + SIDEBAR_TAB_H
                        + body_h
                        + strip_h
                        + SIDEBAR_ROW_PAD / 2.0
                        + k as f32 * SIDEBAR_ROW_H;
                    if ry + SIDEBAR_ROW_H > bottom {
                        break;
                    }
                    rows.push((
                        i,
                        id.clone(),
                        (tab_x + 8.0, ry, tab_w - 16.0, SIDEBAR_ROW_H),
                    ));
                }
            }
            y += h + SIDEBAR_TAB_GAP;
        }
        // `+` 는 목록 꼬리가 아니라 하단 트레이의 왼쪽 칸이다 — 세션이 늘어도 자리가
        // 안 움직인다. 트레이가 없는 배치(top 탭·사이드바 접힘)에서는 어차피 이
        // 분기를 안 타므로 폴백은 목록 꼬리 그대로.
        let plus = self
            .sidebar_tray_rects(win_h)
            .map_or_else(|| (tab_x, y, tab_w, 28.0), |t| t.plus);
        (tabs, closes, plus, rows, mini, undock)
    }
}

/// 목록을 끝까지 내렸을 때의 스크롤 위치(px). 넘치지 않으면 0.
///
/// 예전엔 「마지막 방이 보이는 최소 시작 **인덱스**」였다. 카드 단위로만 굴릴 때는
/// 그걸로 충분했지만, 픽셀로 흐르는 지금은 카드 중간에서 멈추는 자리가 정상이라
/// 인덱스로는 표현이 안 된다. 칸보다 큰 카드 한 장의 아래쪽을 들여다보는 것도
/// 인덱스로는 방법이 아예 없었다.
pub(crate) fn max_scroll_for(heights: &[f32], avail_h: f32, gap: f32) -> f32 {
    let n = heights.len();
    if n == 0 {
        return 0.0;
    }
    let content = heights.iter().sum::<f32>() + gap * n.saturating_sub(1) as f32;
    (content - avail_h).max(0.0)
}

#[cfg(test)]
mod sidebar_scroll_tests {
    use super::max_scroll_for;

    // 실측 치수: 접힌 카드 54, 카드 사이 3, pane 2개를 편 카드는 124
    // (54 + (36+13*2).clamp(46,150) + 8).
    const FOLDED: f32 = 54.0;
    const OPEN2: f32 = 124.0;
    const GAP: f32 = 3.0;

    fn content(heights: &[f32]) -> f32 {
        heights.iter().sum::<f32>() + GAP * heights.len().saturating_sub(1) as f32
    }

    /// 예전 계산 — 접힌 높이로 칸을 나눠 시작 인덱스의 한계를 냈다. 무엇이
    /// 어긋났는지 비교하려고 남긴다.
    fn old_max_first(n: usize, avail_h: f32) -> usize {
        let stride = FOLDED + GAP;
        let n_vis = n.min((((avail_h + GAP) / stride) as usize).max(1));
        n.saturating_sub(n_vis)
    }

    #[test]
    fn expanded_rooms_stay_reachable() {
        // 방 10개 중 뒤 3개를 폈다. 접힌 높이로는 13칸이 들어가니 옛 계산은
        // "다 보인다"고 판단하지만, 실제 높이 합은 777 로 752 를 넘는다.
        let mut heights = vec![FOLDED; 7];
        heights.extend([OPEN2; 3]);
        let avail = 752.0;
        let total = content(&heights);
        assert!(
            total > avail,
            "전제가 깨졌다: 목록이 칸에 다 들어간다 ({total} <= {avail})"
        );
        assert_eq!(
            old_max_first(heights.len(), avail),
            0,
            "옛 계산이 0 이 아니면 이 테스트가 재현하려는 버그가 아니다"
        );

        let max = max_scroll_for(&heights, avail, GAP);
        assert!(
            max > 0.0,
            "펼친 방 때문에 목록이 넘치는데 한계가 0 이면 아래쪽 방에 영영 못 닿는다"
        );
        // 끝까지 내리면 마지막 카드 바닥이 칸 바닥에 딱 온다 — 더도 덜도 아니어야
        // 한다. 모자라면 꼬리가 잘리고, 넘치면 빈 자리가 열린다.
        assert!(
            (max + avail - total).abs() < 0.01,
            "끝이 어긋난다: {max} + {avail} vs {total}"
        );
    }

    #[test]
    fn fitting_list_never_scrolls() {
        assert_eq!(max_scroll_for(&[FOLDED; 5], 752.0, GAP), 0.0);
        assert_eq!(max_scroll_for(&[FOLDED], 752.0, GAP), 0.0);
        assert_eq!(max_scroll_for(&[], 752.0, GAP), 0.0);
    }

    #[test]
    fn a_card_taller_than_the_column_scrolls_to_its_bottom() {
        // 카드 한 장이 칸보다 커도 그 아래쪽까지 볼 수 있어야 한다. 인덱스로 셀
        // 때는 카드 안을 들여다볼 방법이 아예 없었다 — 시작점이 그 카드 머리에
        // 묶여 있었다.
        let heights = [FOLDED, 400.0];
        let avail = 300.0;
        let max = max_scroll_for(&heights, avail, GAP);
        assert!(max > 0.0);
        assert!((max + avail - content(&heights)).abs() < 0.01);
    }

    #[test]
    fn every_room_is_reachable_by_scrolling() {
        // 어떤 조합이든 한계까지 내리면 마지막 방 바닥이 칸 안에 들어와야 한다.
        for open_at in 0..8usize {
            let mut heights = vec![FOLDED; 8];
            heights[open_at] = OPEN2;
            heights[(open_at + 3) % 8] = OPEN2;
            let avail = 400.0;
            let max = max_scroll_for(&heights, avail, GAP);
            assert!(
                max + avail >= content(&heights) - 0.01,
                "open_at={open_at}: 한계까지 내려도 꼬리가 넘친다"
            );
        }
    }
}
