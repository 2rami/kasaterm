type Rect = (f32, f32, f32, f32);

pub(super) struct PopoverLayout {
    pub frame: Rect,
    pub body_height: f32,
    pub scroll_max: f32,
    pub above: bool,
}

pub(super) struct FlyoutLayout {
    pub frame: Rect,
    pub corridor: Option<Rect>,
    pub body_height: f32,
    pub scroll_max: f32,
}

pub(super) fn flyout_layout(
    viewport: (f32, f32),
    parent: Rect,
    provider: Rect,
    width: f32,
    fixed_height: f32,
    content_height: f32,
) -> FlyoutLayout {
    let width = width.min((viewport.0 - 16.0).max(0.0));
    let height = (fixed_height + content_height).min((viewport.1 - 16.0).max(0.0));
    let right = parent.0 + parent.2 + 4.0;
    let left = parent.0 - width - 4.0;
    let (x, corridor) = if right + width <= viewport.0 - 8.0 {
        (right, Some((right - 4.0, provider.1, 4.0, provider.3)))
    } else if left >= 8.0 {
        (left, Some((parent.0 - 4.0, provider.1, 4.0, provider.3)))
    } else {
        ((viewport.0 - width - 8.0).max(8.0), None)
    };
    let y = provider.1.clamp(8.0, (viewport.1 - height - 8.0).max(8.0));
    let body_height = (height - fixed_height).max(0.0);
    FlyoutLayout {
        frame: (x, y, width, height),
        corridor,
        body_height,
        scroll_max: (content_height - body_height).max(0.0),
    }
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
    fn flyout_prefers_right_and_connects_only_provider_row() {
        let p = flyout_layout(
            (1200.0, 800.0),
            (8.0, 300.0, 380.0, 300.0),
            (8.0, 350.0, 380.0, 40.0),
            380.0,
            40.0,
            200.0,
        );
        assert_eq!(p.frame, (392.0, 350.0, 380.0, 240.0));
        assert_eq!(p.corridor, Some((388.0, 350.0, 4.0, 40.0)));
    }

    #[test]
    fn flyout_flips_left_when_right_is_full() {
        let p = flyout_layout(
            (1200.0, 800.0),
            (800.0, 300.0, 380.0, 300.0),
            (800.0, 350.0, 380.0, 40.0),
            380.0,
            40.0,
            200.0,
        );
        assert_eq!(p.frame.0, 416.0);
        assert_eq!(p.corridor, Some((796.0, 350.0, 4.0, 40.0)));
    }

    #[test]
    fn narrow_flyout_overlaps_with_bounded_scroll_body() {
        let p = flyout_layout(
            (300.0, 400.0),
            (8.0, 100.0, 284.0, 272.0),
            (8.0, 150.0, 284.0, 40.0),
            380.0,
            40.0,
            2000.0,
        );
        assert_eq!(p.frame, (8.0, 8.0, 284.0, 384.0));
        assert_eq!(p.corridor, None);
        assert_eq!(p.body_height, 344.0);
        assert_eq!(p.scroll_max, 1656.0);
    }

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
