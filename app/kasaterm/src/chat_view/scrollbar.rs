//! 대화 보기 스크롤바 — 대화 줄 칸 오른쪽 끝 한 칸. 스크롤을 이 보기가 쥐고 있어 누름·끌기가 바로 그 줄로 간다
//! (풀스크린 claude 칸 위에 그리던 막대는 claude 에 부탁해야 해 느렸다 — `docs/design.md` 「풀스크린 claude 프롬프트 이동」).
//!
//! 칸 경계의 나누기 손잡이가 경계 양쪽 6px 를 먼저 받으므로 막대는 여백이 아니라 격자 안에 둔다. 대화 줄은 그만큼
//! 한 칸 좁게 접는다(`BAR_COLS`).

use super::*;

/// 대화 줄을 접을 때 비우는 칸 수 — 그 자리가 막대다.
pub(crate) const BAR_COLS: usize = 1;

const BAR_W: f32 = 3.5;
const BAR_W_ACTIVE: f32 = 5.0;
const THUMB_MIN_H: f32 = 24.0;

/// 한 대화 칸의 막대 자리(논리 px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Bar {
    /// 막대 가운데 x.
    x: f32,
    /// 누름 자리 왼쪽·폭 — 비운 마지막 칸 전부.
    hit: (f32, f32),
    track: (f32, f32),
    thumb: (f32, f32),
}

/// 대화가 한 화면에 다 들면 막대가 없다.
pub(super) fn bar(d: &Drawn) -> Option<Bar> {
    let v = d.view;
    let (cw, ch) = d.cell;
    if v.total <= v.hist || v.cols <= BAR_COLS || cw <= 0.0 || ch <= 0.0 {
        return None;
    }
    let (ty, th) = (d.rect.1, v.hist as f32 * ch);
    if th <= THUMB_MIN_H {
        return None;
    }
    let scale = th / v.total as f32;
    let exact = v.hist as f32 * scale;
    let h = exact.max(THUMB_MIN_H).min(th);
    // 최소 높이로 키운 만큼은 위아래로 나눠 덮고, 홈 밖으로 나가지 않게 민다.
    let y = (ty + v.first as f32 * scale - (h - exact) / 2.0).clamp(ty, ty + th - h);
    let left = d.rect.0 + (v.cols - BAR_COLS) as f32 * cw;
    let width = BAR_COLS as f32 * cw;
    Some(Bar { x: left + width / 2.0, hit: (left, width), track: (ty, th), thumb: (y, h) })
}

impl Bar {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.hit.0 && x <= self.hit.0 + self.hit.1 && y >= self.track.0 && y <= self.track.0 + self.track.1
    }

    /// 손잡이 가운데를 `y` 에 둘 때 맨 위에 설 대화 줄. 그리는 쪽(`bar`)과 같은 비례다.
    fn first_at(&self, y: f32, view: &select::View) -> usize {
        let (ty, th) = self.track;
        let frac = ((y - ty) / th.max(1.0)).clamp(0.0, 1.0);
        let center = frac * view.total as f32 - view.hist as f32 / 2.0;
        (center.round().max(0.0) as usize).min(view.total.saturating_sub(view.hist))
    }
}

/// `back`(맨 아래에서 올라간 줄 수)으로 — 맨 위 줄 `first` 를 세운다.
fn back_for(first: usize, view: &select::View) -> usize {
    view.total.saturating_sub(view.hist).saturating_sub(first)
}

/// 이번 프레임에 대화로 그린 칸마다 막대를 그린다. 쉴 때는 가는 손잡이만, 올리거나 끄는 중엔 굵어지고 홈이 깔린다.
pub(crate) fn paint_scrollbars(g: &mut crate::gpu::GpuRenderer, views: &ChatViews) {
    let active = views.bar_drag.as_ref().map(|(id, _)| id.as_str()).or(views.bar_hover.as_deref());
    for (id, d) in views.drawn.borrow().iter() {
        let Some(b) = bar(d) else { continue };
        let on = active == Some(id.as_str());
        let w = if on { BAR_W_ACTIVE } else { BAR_W };
        if on {
            g.rect(b.x - w / 2.0, b.track.0, w, b.track.1, theme::with_alpha(theme::text(), 0x14));
        }
        crate::pill_rect(g, b.x - w / 2.0, b.thumb.0, w, b.thumb.1, theme::with_alpha(theme::text(), if on { 0x99 } else { 0x66 }));
    }
}

impl App {
    fn chat_bar_at(&self, x: f32, y: f32) -> Option<(String, Bar, select::View)> {
        let drawn = self.chat_view.drawn.borrow();
        drawn.iter().find_map(|(id, d)| bar(d).filter(|b| b.contains(x, y)).map(|b| (id.clone(), b, d.view)))
    }

    /// 막대 누름 — 손잡이면 잡은 자리를 기억해 끌고, 홈이면 그 자리가 가운데 오게 옮긴 뒤 끈다.
    pub(super) fn chat_bar_press(&mut self, x: f32, y: f32) -> bool {
        let Some((id, b, view)) = self.chat_bar_at(x, y) else { return false };
        let (ty, th) = b.thumb;
        let grab = if y >= ty && y <= ty + th {
            y - (ty + th / 2.0)
        } else {
            if let Some(pane) = self.chat_view.panes.get(&id) {
                pane.back.set(back_for(b.first_at(y, &view), &view));
            }
            0.0
        };
        self.chat_view.bar_drag = Some((id, grab));
        self.chrome_dirty = true;
        true
    }

    /// 손잡이 끌기 — 잡은 자리가 커서를 따라간다. 끌고 있지 않으면 false.
    pub(super) fn chat_bar_drag(&mut self) -> bool {
        let (x, y) = self.cursor_px;
        let Some((id, grab)) = self.chat_view.bar_drag.clone() else {
            let over = self.chat_bar_at(x, y).map(|(id, ..)| id);
            if over != self.chat_view.bar_hover {
                self.chat_view.bar_hover = over;
                self.chrome_dirty = true;
            }
            return false;
        };
        let at = self.chat_view.drawn.borrow().get(&id).and_then(|d| bar(d).map(|b| (b, d.view)));
        if let (Some((b, view)), Some(pane)) = (at, self.chat_view.panes.get(&id)) {
            pane.back.set(back_for(b.first_at(y - grab, &view), &view));
            self.chrome_dirty = true;
        }
        true
    }

    pub(super) fn chat_bar_release(&mut self) -> bool {
        let had = self.chat_view.bar_drag.take().is_some();
        if had {
            self.chrome_dirty = true;
        }
        had
    }

    /// 리그 `bar=0.1>0.7` — 홈의 그 비율 자리를 눌러 다른 비율까지 끌고 놓는다(사람 손과 같은 함수).
    pub(super) fn chat_bar_auto(&mut self, from: f32, to: f32) {
        let Some((x, b)) = self.chat_view.drawn.borrow().values().find_map(|d| bar(d).map(|b| (b.x, b))) else {
            eprintln!("[autochat] bar: 막대가 이번 프레임에 없다");
            return;
        };
        let at = |f: f32| b.track.0 + b.track.1 * f;
        let pressed = self.chat_bar_press(x, at(from));
        for i in 1..=6 {
            self.cursor_px = (x, at(from + (to - from) * i as f32 / 6.0));
            self.chat_bar_drag();
        }
        self.chat_bar_release();
        eprintln!("[autochat] bar {from}>{to} pressed={pressed}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawn(total: usize, first: usize, hist: usize) -> Drawn {
        Drawn {
            rect: (100.0, 50.0, 800.0, 600.0),
            cell: (8.0, 20.0),
            hist_rows: hist,
            toggles: Vec::new(),
            rows: Vec::new(),
            cursor: None,
            caret: None,
            view: select::View { first, hist, total, bottom: 5, cols: 100 },
        }
    }

    #[test]
    fn the_thumb_is_proportional_and_sits_in_the_last_column() {
        let b = bar(&drawn(400, 100, 20)).unwrap();
        assert_eq!(b.track, (50.0, 400.0));
        assert_eq!(b.thumb, (148.0, 24.0), "400줄 중 20줄이면 20px — 최소 높이로 제자리 둘레에 키운다");
        assert_eq!(b.hit, (100.0 + 99.0 * 8.0, 8.0));
        assert!(bar(&drawn(20, 0, 20)).is_none(), "everything fits — no bar");
    }

    #[test]
    fn pressing_or_dragging_maps_back_to_the_same_line() {
        let d = drawn(400, 100, 20);
        let b = bar(&d).unwrap();
        // 손잡이 가운데 = 그리는 쪽과 같은 자리.
        assert_eq!(b.first_at(b.thumb.0 + b.thumb.1 / 2.0, &d.view), 100);
        assert_eq!(b.first_at(b.track.0 - 30.0, &d.view), 0);
        assert_eq!(b.first_at(b.track.0 + b.track.1 + 30.0, &d.view), 380);
        assert_eq!(back_for(380, &d.view), 0, "맨 아래 — 새 줄을 따라간다");
        assert_eq!(back_for(0, &d.view), 380);
    }
}
