use super::Density;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SidebarMetrics {
    pub width: f32,
    pub density: Density,
}

impl SidebarMetrics {
    pub(crate) fn new(width: f32) -> Self {
        Self { width, density: Density::sidebar(width) }
    }

    pub(crate) fn row_h(self) -> f32 {
        match self.density {
            Density::Icon | Density::Compact => 28.0,
            Density::Full => crate::sidebar_pulse::LIST_ROW_H,
            Density::Wide => 48.0,
            Density::Spacious => 56.0,
        }
    }

    pub(crate) fn retained_h(self) -> f32 {
        if self.density.is_wide() { 40.0 } else { crate::SIDEBAR_ROW_H }
    }

    pub(crate) fn body_h(self, panes: usize, listed: bool) -> f32 {
        if listed {
            panes as f32 * self.row_h() + crate::sidebar_pulse::LIST_TOP_GAP
        } else {
            let scale = if self.density.is_wide() { (self.width / crate::SIDEBAR_W).min(2.2) } else { 1.0 };
            ((36.0 + 13.0 * panes as f32).clamp(46.0, 150.0) * scale).round()
        }
    }

    pub(crate) fn undocked(self, count: usize) -> (usize, f32, f32, f32) {
        if count == 0 { return (1, 0.0, 0.0, 0.0); }
        let available = (self.width - 2.0 * crate::SIDEBAR_TAB_INSET - 20.0).max(0.0);
        let minimum = if self.density.is_wide() { 120.0 } else { 26.0 };
        let columns = (((available + 3.0) / (minimum + 3.0)) as usize).max(1).min(count);
        let width = ((available - 3.0 * columns.saturating_sub(1) as f32) / columns as f32).max(0.0);
        let width = if self.density.is_wide() { width } else { width.min(40.0) };
        let row_h = if self.density.is_wide() { 64.0 } else { crate::UNDOCK_STRIP_H };
        (columns, width, row_h, count.div_ceil(columns) as f32 * row_h)
    }

    pub(crate) fn map_label(self, width: f32, height: f32) -> bool {
        self.density.is_wide() && width >= 80.0 && height >= 60.0
    }

    pub(crate) fn face_box(self, r: (f32, f32, f32, f32)) -> (f32, f32, f32) {
        let (x, y, w, h) = r;
        if !self.map_label(w, h) { return crate::render::minimap_face_box(x, y, w, h); }
        let max_face = if self.density == Density::Spacious { 48.0 } else { 40.0 };
        let face = (w.min(h - 28.0) - 8.0).clamp(10.0, max_face);
        (x + (w - face) / 2.0, y + (h - 28.0 - face) / 2.0, face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_breakpoints_leave_other_columns_unchanged() {
        for (w, density, row) in [(104.0, Density::Icon, 28.0), (124.0, Density::Compact, 28.0),
            (168.0, Density::Full, 40.0), (319.0, Density::Full, 40.0),
            (320.0, Density::Wide, 48.0), (480.0, Density::Spacious, 56.0)] {
            let m = SidebarMetrics::new(w);
            assert_eq!(m.density, density);
            assert_eq!(m.body_h(6, true), 6.0 * row + crate::sidebar_pulse::LIST_TOP_GAP);
        }
        assert_eq!(Density::of(520.0, 168.0, 124.0), Density::Full);
    }

    #[test]
    fn detached_tiles_wrap_inside_the_card_and_reserve_every_row() {
        for width in [104.0, 140.0, 240.0, 320.0, 480.0, 520.0] {
            for count in 1..30 {
                let (columns, cell, row, height) = SidebarMetrics::new(width).undocked(count);
                assert!(columns as f32 * cell + columns.saturating_sub(1) as f32 * 3.0 <= width - 36.0 + 0.01);
                assert_eq!(height, count.div_ceil(columns) as f32 * row);
            }
        }
    }

    #[test]
    fn wider_maps_gain_readable_height_and_keep_labels_below_faces() {
        let base = SidebarMetrics::new(240.0).body_h(6, false);
        let wide = SidebarMetrics::new(360.0);
        let spacious = SidebarMetrics::new(520.0);
        assert!(wide.body_h(6, false) > base);
        assert!(spacious.body_h(6, false) > wide.body_h(6, false));
        for m in [wide, spacious] {
            let (_, y, face) = m.face_box((0.0, 0.0, 100.0, 70.0));
            assert!(y >= 0.0 && y + face < 70.0 - 28.0);
        }
    }
}
