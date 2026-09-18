type Rect = (f32, f32, f32, f32);

pub(super) struct PopoverLayout {
    pub frame: Rect,
    pub body_height: f32,
    pub scroll_max: f32,
    pub above: bool,
}

pub(super) fn layout(
    viewport: (f32, f32),
    anchor: Rect,
    width: f32,
    fixed_height: f32,
    content_height: f32,
) -> PopoverLayout {
    let (ax, ay, aw, ah) = anchor;
    let width = width.min((viewport.0 - 16.0).max(0.0));
    let above_room = (ay - 8.0).max(0.0);
    let below_room = (viewport.1 - ay - ah - 8.0).max(0.0);
    let above = above_room >= below_room;
    let available = if above { above_room } else { below_room };
    let height = (fixed_height + content_height).min(available);
    let body_height = (height - fixed_height).max(0.0);
    let x = (ax + aw - width).clamp(8.0, (viewport.0 - width - 8.0).max(8.0));
    let y = if above { ay - height } else { ay + ah };
    PopoverLayout {
        frame: (x, y, width, height),
        body_height,
        scroll_max: (content_height - body_height).max(0.0),
        above,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_trigger_and_panel_share_an_edge() {
        let p = layout(
            (1200.0, 800.0),
            (980.0, 772.0, 140.0, 28.0),
            380.0,
            127.0,
            240.0,
        );
        assert!(p.above);
        assert_eq!(p.frame.1 + p.frame.3, 772.0);
        assert_eq!(p.frame.0 + p.frame.2, 1120.0);
        assert_eq!(p.scroll_max, 0.0);
    }

    #[test]
    fn narrow_window_keeps_the_full_panel_inside() {
        let p = layout(
            (300.0, 500.0),
            (250.0, 472.0, 42.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert_eq!(p.frame.0, 8.0);
        assert_eq!(p.frame.2, 284.0);
        assert!(p.frame.0 + p.frame.2 <= 292.0);
    }

    #[test]
    fn long_accounts_scroll_without_moving_fixed_chrome() {
        let p = layout(
            (800.0, 400.0),
            (620.0, 372.0, 100.0, 28.0),
            380.0,
            127.0,
            2000.0,
        );
        assert_eq!(p.frame.1, 8.0);
        assert_eq!(p.frame.3 - p.body_height, 127.0);
        assert_eq!(p.scroll_max + p.body_height, 2000.0);
    }

    #[test]
    fn upper_trigger_opens_below_without_a_gap() {
        let p = layout(
            (800.0, 600.0),
            (10.0, 50.0, 120.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert!(!p.above);
        assert_eq!(p.frame.1, 78.0);
        assert_eq!(p.frame.0, 8.0);
    }
}
