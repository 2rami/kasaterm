#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Order {
    pub(super) keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Section {
    pub(super) key: String,
    pub(super) top: f32,
    pub(super) height: f32,
}

impl Order {
    pub(super) fn new(keys: impl IntoIterator<Item = String>) -> Self {
        let mut order = Self::default();
        for key in keys {
            if !order.keys.contains(&key) { order.keys.push(key); }
        }
        order
    }

    pub(super) fn visible(&self, live: &[String]) -> Vec<String> {
        Self::new(self.keys.iter().filter(|key| live.contains(key)).chain(live.iter()).cloned()).keys
    }

    pub(super) fn moved(&self, live: &[String], source: &str, target: &str, side: Side) -> Option<Self> {
        if source == target || !live.iter().any(|key| key == source) || !live.iter().any(|key| key == target) {
            return None;
        }
        // A temporarily missing device must retain its saved place when it returns.
        let mut next = Self::new(self.keys.iter().chain(live).cloned());
        next.keys.retain(|key| key != source);
        let index = next.keys.iter().position(|key| key == target)? + usize::from(side == Side::After);
        next.keys.insert(index, source.to_string());
        (next.visible(live) != self.visible(live)).then_some(next)
    }
}

pub(super) fn layout(order: &Order, sizes: &[(String, f32)]) -> Vec<Section> {
    let live: Vec<_> = sizes.iter().map(|(key, _)| key.clone()).collect();
    let mut top = 0.0;
    order.visible(&live).into_iter().map(|key| {
        let height = sizes.iter().find(|(id, _)| *id == key).map_or(0.0, |(_, h)| h.max(0.0));
        let section = Section { key, top, height };
        top += height;
        section
    }).collect()
}

pub(super) fn insertion(sections: &[Section], source: &str, y: f32) -> Option<(String, Side, f32)> {
    let mut last = None;
    for section in sections.iter().filter(|section| section.key != source) {
        if y < section.top + section.height / 2.0 {
            return Some((section.key.clone(), Side::Before, section.top));
        }
        last = Some((section.key.clone(), Side::After, section.top + section.height));
    }
    last
}

pub(super) fn scroll_speed(cursor: (f32, f32), viewport: (f32, f32, f32, f32)) -> f32 {
    let (x, y, w, h) = viewport;
    if h <= 0.0 || cursor.0 < x || cursor.0 >= x + w || cursor.1 < y || cursor.1 >= y + h { return 0.0; }
    let edge = 24.0_f32.min(h / 2.0);
    if cursor.1 < y + edge { -360.0 * (1.0 - (cursor.1 - y) / edge) }
    else if cursor.1 > y + h - edge { 360.0 * (1.0 - (y + h - cursor.1) / edge) }
    else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> Vec<String> { values.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn local_and_remote_devices_move_in_both_directions() {
        let live = keys(&["", "mini", "pc"]);
        let order = Order::default().moved(&live, "pc", "", Side::Before).unwrap();
        assert_eq!(order.visible(&live), keys(&["pc", "", "mini"]));
        let order = order.moved(&live, "", "mini", Side::After).unwrap();
        assert_eq!(order.visible(&live), keys(&["pc", "mini", ""]));
        assert!(order.moved(&live, "pc", "mini", Side::Before).is_none());
    }

    #[test]
    fn snapshot_refresh_and_new_devices_do_not_reset_saved_order() {
        let order = Order::new(keys(&["pc", "", "mini", "pc"]));
        assert_eq!(order.visible(&keys(&["", "mini", "pc", "new"])), keys(&["pc", "", "mini", "new"]));
        assert_eq!(order.visible(&keys(&["", "mini"])), keys(&["", "mini"]));
        assert_eq!(order.visible(&keys(&["", "mini", "pc"])), keys(&["pc", "", "mini"]));
    }

    #[test]
    fn reordering_does_not_discard_missing_device_preferences() {
        let order = Order::new(keys(&["", "missing", "mini", "pc"]));
        let moved = order.moved(&keys(&["", "mini", "pc"]), "pc", "", Side::Before).unwrap();
        assert_eq!(moved.keys, keys(&["pc", "", "missing", "mini"]));
        assert!(order.moved(&keys(&["", "mini"]), "pc", "", Side::Before).is_none());
    }

    #[test]
    fn insertion_uses_whole_device_bounds_and_handles_both_ends() {
        let sections = layout(&Order::default(), &[("".into(), 200.0), ("mini".into(), 36.0), ("pc".into(), 400.0)]);
        assert_eq!(insertion(&sections, "pc", -10.0), Some(("".into(), Side::Before, 0.0)));
        assert_eq!(insertion(&sections, "", 500.0), Some(("pc".into(), Side::After, 636.0)));
        let moved = layout(&Order::new(keys(&["pc", "", "mini"])), &[("".into(), 200.0), ("mini".into(), 36.0), ("pc".into(), 400.0)]);
        assert_eq!(moved[1].top, 400.0);
        assert_eq!(moved[2].top + moved[2].height, 636.0);
    }

    #[test]
    fn edge_scroll_stays_inside_the_sidebar() {
        let view = (0.0, 100.0, 240.0, 400.0);
        assert!(scroll_speed((120.0, 101.0), view) < 0.0);
        assert!(scroll_speed((120.0, 499.0), view) > 0.0);
        assert_eq!(scroll_speed((120.0, 300.0), view), 0.0);
        assert_eq!(scroll_speed((250.0, 499.0), view), 0.0);
        assert_eq!(scroll_speed((120.0, 501.0), view), 0.0);
    }
}
