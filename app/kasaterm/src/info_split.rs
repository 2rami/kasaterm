//! Info 열의 위아래 나눔 — 위는 「지금 보는 칸」 카드와 「내 프롬프트」, 아래는 「결과물」·「모든 방」. 사이 경계를
//! 끌어 두 영역의 높이 비율을 정하고, 영역마다 따로 굴린다.
//!
//! 비율은 경계를 뺀 칼럼 높이에서 위 영역이 차지하는 몫이다. 설정 파일 옆 `info-layout.json` 에 남는다 — 검증
//! 앱은 설정 파일을 격리하므로(`KASATERM_SETTINGS_FILE`) 사람의 값을 안 건드린다. 한 영역의 내용이 제 몫보다
//! 짧으면 남는 자리를 다른 영역에 준다. 그래서 경계는 넘치는 쪽이 있을 때만 서고, 두 영역 모두 빈자리가 안
//! 생기는 범위에서만 움직인다. 칼럼이 두 최소 높이를 못 담으면 나누지 않고 한 흐름으로 굴린다.

use std::path::PathBuf;

type Rect = (f32, f32, f32, f32);

/// 경계 손잡이 띠 높이 — 가운데에 선 하나.
pub(crate) const GRIP_H: f32 = 8.0;
/// 위 영역 최소 — 카드 머리와 줄 셋.
const MIN_UPPER: f32 = 96.0;
/// 아래 영역 최소 — 묶음 머리와 줄 둘.
const MIN_LOWER: f32 = 74.0;
/// 끌어 본 적 없을 때 위 영역 몫.
const DEFAULT_SHARE: f32 = 0.6;

#[derive(Default)]
pub(crate) struct Split {
    /// 사람이 끌어 정한 위 영역 몫. `None` 이면 `DEFAULT_SHARE`.
    share: Option<f32>,
    loaded: bool,
    /// 끄는 중 — (시작 커서 y, 시작 위 영역 높이, 움직였나). 델타를 시작점에서 재는 건 다른 경계 끌기와 같은
    /// 이유다 — 누적하면 끝에 걸린 뒤 커서를 되돌려도 값이 안 따라온다.
    drag: Option<(f32, f32, bool)>,
    /// 직전 프레임의 경계 손잡이 — 렌더가 쓰고 마우스가 읽는다. 나누지 않은 프레임이면 `None`.
    pub(crate) grip: Option<Rect>,
    /// 직전 프레임의 아래 영역 — 휠이 어느 영역을 굴릴지 가른다.
    lower: Option<Rect>,
    /// 직전 프레임의 위 영역 높이, 그 높이가 움직일 수 있는 범위, 경계를 뺀 칼럼 높이.
    upper_h: f32,
    range: (f32, f32),
    room: f32,
    pub(crate) lower_scroll: f32,
}

/// 위 영역 높이와 그 높이가 움직일 수 있는 범위. `None` 이면 나누지 않는다 — 한 흐름에 다 들어가거나, 칼럼이
/// 두 최소 높이를 못 담는다. 두 영역 모두 제 내용보다 커지지 않게 잡는다 — 최소 높이도 내용이 더 짧으면 내용까지다.
pub(crate) fn place(avail: f32, upper: f32, lower: f32, share: Option<f32>) -> Option<(f32, (f32, f32))> {
    let room = avail - GRIP_H;
    if upper + lower <= avail || room < MIN_UPPER + MIN_LOWER {
        return None;
    }
    let hi = upper.min(room - MIN_LOWER.min(lower));
    let lo = (room - lower).max(MIN_UPPER.min(upper)).min(hi);
    Some(((share.unwrap_or(DEFAULT_SHARE) * room).clamp(lo, hi), (lo, hi)))
}

fn layout_path() -> Option<PathBuf> {
    Some(crate::socket::settings_file_path()?.with_file_name("info-layout.json"))
}

fn load() -> Option<f32> {
    let text = std::fs::read_to_string(layout_path()?).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["prompt_share"].as_f64().map(|s| s as f32).filter(|s| s.is_finite() && (0.0..=1.0).contains(s))
}

fn save(share: f32) {
    if let Some(path) = layout_path() {
        let _ = std::fs::write(path, serde_json::json!({ "prompt_share": share }).to_string());
    }
}

impl Split {
    /// 이번 프레임의 위 영역 높이. 처음 부를 때 저장된 비율을 읽는다. 나누지 않으면 손잡이·아래 영역을 비운다.
    pub(crate) fn layout(&mut self, prompts: bool, avail: f32, upper: f32, lower: f32) -> Option<f32> {
        if !self.loaded {
            self.loaded = true;
            self.share = load();
        }
        let placed = place(avail, upper, lower, self.share).filter(|_| prompts);
        match placed {
            Some((h, range)) => {
                self.upper_h = h;
                self.range = range;
                self.room = avail - GRIP_H;
            }
            None => {
                self.grip = None;
                self.lower = None;
                self.lower_scroll = 0.0;
            }
        }
        placed.map(|(h, _)| h)
    }

    pub(crate) fn set_frame(&mut self, grip: Rect, lower: Rect) {
        self.grip = Some(grip);
        self.lower = Some(lower);
    }

    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// 경계를 끌 수 있나 — 한쪽이 제 내용을 다 보이고 있으면 내줄 자리가 없어 선만 남는다.
    pub(crate) fn movable(&self) -> bool {
        self.range.1 - self.range.0 > 0.5
    }

    fn on_grip(&self, cx: f32, cy: f32) -> bool {
        self.movable() && self.grip.is_some_and(|r| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
    }
}

impl crate::state::InfoState {
    /// 휠이 굴릴 스크롤 — 아래 영역 위면 그 영역, 아니면 위 영역(나누지 않았으면 칼럼 전체).
    pub(crate) fn wheel_scroll(&mut self, cy: f32) -> &mut f32 {
        if self.split.lower.is_some_and(|r| cy >= r.1 && cy <= r.1 + r.3) {
            &mut self.split.lower_scroll
        } else {
            &mut self.scroll
        }
    }
}

impl crate::App {
    fn info_split_live(&self) -> bool {
        self.git.col_visible && self.info.runtime_page() && self.info.ctx_menu.is_none()
    }

    /// 커서가 경계 위인가 — 끌 수 있다는 커서를 띄운다.
    pub(crate) fn on_info_split(&self, cx: f32, cy: f32) -> bool {
        self.info_split_live() && (self.info.split.dragging() || self.info.split.on_grip(cx, cy))
    }

    pub(crate) fn info_split_press(&mut self, cx: f32, cy: f32) -> bool {
        if !self.info_split_live() || !self.info.split.on_grip(cx, cy) {
            return false;
        }
        self.info.split.drag = Some((cy, self.info.split.upper_h, false));
        self.chrome_dirty = true;
        true
    }

    /// 끄는 중이면 `Some(경계가 움직였나)`.
    pub(crate) fn info_split_drag(&mut self, cy: f32) -> Option<bool> {
        let split = &mut self.info.split;
        let (start_y, start_h, _) = split.drag?;
        let (lo, hi) = split.range;
        let h = (start_h + (cy - start_y)).clamp(lo, hi.max(lo));
        let moved = (h - split.upper_h).abs() > 0.5 && split.room > 0.0;
        if moved {
            split.share = Some(h / split.room);
            split.upper_h = h;
            split.drag = Some((start_y, start_h, true));
            self.chrome_dirty = true;
        }
        Some(moved)
    }

    /// 끌기 끝 — 움직였으면 비율을 남긴다.
    pub(crate) fn info_split_release(&mut self) -> bool {
        let Some((_, _, moved)) = self.info.split.drag.take() else { return false };
        if moved {
            if let Some(share) = self.info.split.share {
                save(share);
            }
        }
        self.chrome_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_column_splits_only_when_something_overflows_and_it_is_tall_enough() {
        assert_eq!(place(600.0, 200.0, 300.0, None), None, "다 들어가면 한 흐름");
        assert_eq!(place(170.0, 400.0, 400.0, None), None, "두 최소 높이를 못 담는 낮은 칼럼은 한 흐름");
        let (h, range) = place(600.0, 900.0, 900.0, None).unwrap();
        assert_eq!(h, (600.0 - GRIP_H) * DEFAULT_SHARE);
        assert_eq!(range, (MIN_UPPER, 600.0 - GRIP_H - MIN_LOWER));
    }

    #[test]
    fn a_short_side_lends_its_spare_room_and_the_drag_stays_inside_the_contents() {
        let room = 600.0 - GRIP_H;
        // 방을 접어 아래가 짧으면 아래는 제 내용만큼만, 나머지는 프롬프트가 쓴다.
        let (h, (lo, hi)) = place(600.0, 2000.0, 100.0, Some(0.3)).unwrap();
        assert_eq!((h, lo, hi), (room - 100.0, room - 100.0, room - MIN_LOWER));
        // 아래 내용이 최소 높이보다 짧으면 그 내용만큼만 — 내줄 자리가 없어 경계는 못 움직인다.
        let (h, (lo, hi)) = place(600.0, 2000.0, 42.0, None).unwrap();
        assert_eq!((h, lo, hi), (room - 42.0, room - 42.0, room - 42.0));
        // 프롬프트가 적으면 위는 제 내용만큼이고 아래가 나머지를 받는다.
        let (h, (_, hi)) = place(600.0, 250.0, 2000.0, Some(0.9)).unwrap();
        assert_eq!((h, hi), (250.0, 250.0));
        // 끌어 정한 비율은 범위 안이면 그대로다.
        let (h, _) = place(600.0, 2000.0, 2000.0, Some(0.25)).unwrap();
        assert_eq!(h, room * 0.25);
        let (h, _) = place(600.0, 2000.0, 2000.0, Some(0.0)).unwrap();
        assert_eq!(h, MIN_UPPER, "끝까지 끌어도 최소 높이는 남는다");
    }

    #[test]
    fn the_wheel_scrolls_the_region_under_the_cursor() {
        let mut info = crate::state::InfoState::default();
        info.split.set_frame((0.0, 300.0, 300.0, GRIP_H), (0.0, 308.0, 300.0, 292.0));
        *info.wheel_scroll(400.0) = 44.0;
        *info.wheel_scroll(100.0) = 22.0;
        assert_eq!((info.split.lower_scroll, info.scroll), (44.0, 22.0));
    }
}
