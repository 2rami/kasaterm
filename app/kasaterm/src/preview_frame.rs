use super::*;

type Rect = (f32, f32, f32, f32);
type Point = (f32, f32);
const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const TOP: u8 = 4;
const BOTTOM: u8 = 8;

#[derive(Clone, Copy)]
struct Drag {
    start: Point,
    rect: Rect,
    edges: u8,
    edited: bool,
}

#[derive(Default)]
pub(crate) struct Frame {
    rect: Option<Rect>,
    bounds: Rect,
    minimum: Point,
    preferred: Point,
    edited: bool,
    drag: Option<Drag>,
}

fn contains((x, y, w, h): Rect, point: Point) -> bool {
    w > 0.0 && h > 0.0 && point.0 >= x && point.0 <= x + w && point.1 >= y && point.1 <= y + h
}

fn cursor_for(edges: u8) -> CursorIcon {
    match edges {
        0 => CursorIcon::Grabbing,
        5 | 10 => CursorIcon::NwseResize,
        6 | 9 => CursorIcon::NeswResize,
        LEFT | RIGHT => CursorIcon::EwResize,
        _ => CursorIcon::NsResize,
    }
}

impl Frame {
    pub(crate) fn layout(&mut self, size: Point, preferred: Option<Point>, minimum: Point) -> Rect {
        let (w, h) = (size.0.max(0.0), size.1.max(0.0));
        let left = 16.0f32.min(w * 0.5);
        let top = (TITLE_HEIGHT + 8.0).min(h * 0.25);
        let bottom = 16.0f32.min(h * 0.5);
        let bounds = (left, top, (w - left * 2.0).max(0.0), (h - top - bottom).max(0.0));
        if bounds != self.bounds { self.drag = None; }
        self.bounds = bounds;
        self.minimum = (minimum.0.min(bounds.2), minimum.1.min(bounds.3));
        let preferred = preferred.unwrap_or((w * 0.5, h * 0.5));
        if self.rect.is_none() || (!self.edited && self.preferred != preferred) {
            self.rect = Some(((w - preferred.0) * 0.5, (h - preferred.1) * 0.5, preferred.0, preferred.1));
        }
        self.preferred = preferred;
        let (x, y, width, height) = self.rect.unwrap();
        let width = width.clamp(self.minimum.0, bounds.2);
        let height = height.clamp(self.minimum.1, bounds.3);
        let rect = (x.clamp(bounds.0, bounds.0 + bounds.2 - width),
            y.clamp(bounds.1, bounds.1 + bounds.3 - height), width, height);
        self.rect = Some(rect);
        rect
    }

    pub(crate) fn rect(&self) -> Option<Rect> { self.rect }

    pub(crate) fn title_contains(&self, point: Point) -> bool {
        self.rect.is_some_and(|(x, y, w, _)| contains((x + 8.0, y + 8.0, (w - 16.0).max(0.0), 40.0), point))
    }

    fn edges(&self, point: Point) -> u8 {
        let Some((x, y, w, h)) = self.rect else { return 0 };
        if w <= 0.0 || h <= 0.0 || !contains((x - 5.0, y - 5.0, w + 10.0, h + 10.0), point) { return 0; }
        let mut edges = 0;
        if (point.0 - x).abs() <= 6.0 { edges |= LEFT; }
        else if (point.0 - x - w).abs() <= 6.0 { edges |= RIGHT; }
        if (point.1 - y).abs() <= 6.0 { edges |= TOP; }
        else if (point.1 - y - h).abs() <= 6.0 { edges |= BOTTOM; }
        edges
    }

    pub(crate) fn cursor(&self, point: Point, move_allowed: bool) -> Option<CursorIcon> {
        if let Some(drag) = self.drag { return Some(cursor_for(drag.edges)); }
        let edges = self.edges(point);
        if edges != 0 { Some(cursor_for(edges)) }
        else if move_allowed { Some(CursorIcon::Grab) }
        else { None }
    }

    fn press(&mut self, point: Point, move_allowed: bool) -> bool {
        let Some(rect) = self.rect else { return false };
        let edges = self.edges(point);
        if edges == 0 && !move_allowed { return false; }
        self.drag = Some(Drag { start: point, rect, edges, edited: self.edited });
        true
    }

    fn motion(&mut self, point: Point) -> bool {
        let Some(drag) = self.drag else { return false };
        let (dx, dy) = (point.0 - drag.start.0, point.1 - drag.start.1);
        if dx.abs() + dy.abs() > 0.5 { self.edited = true; }
        let (x, y, w, h) = drag.rect;
        let (bx, by, bw, bh) = self.bounds;
        let rect = if drag.edges == 0 {
            ((x + dx).clamp(bx, bx + bw - w), (y + dy).clamp(by, by + bh - h), w, h)
        } else {
            let (mut left, mut right, mut top, mut bottom) = (x, x + w, y, y + h);
            if drag.edges & LEFT != 0 { left = (left + dx).clamp(bx, right - self.minimum.0); }
            if drag.edges & RIGHT != 0 { right = (right + dx).clamp(left + self.minimum.0, bx + bw); }
            if drag.edges & TOP != 0 { top = (top + dy).clamp(by, bottom - self.minimum.1); }
            if drag.edges & BOTTOM != 0 { bottom = (bottom + dy).clamp(top + self.minimum.1, by + bh); }
            (left, top, right - left, bottom - top)
        };
        self.rect = Some(rect);
        true
    }

    fn cancel(&mut self) {
        if let Some(drag) = self.drag.take() { self.rect = Some(drag.rect); self.edited = drag.edited; }
    }

    pub(crate) fn event(&mut self, event: &WindowEvent, point: Point, move_allowed: bool) -> bool {
        if matches!(event, WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } | WindowEvent::Destroyed) {
            self.cancel();
            return false;
        }
        if self.drag.is_some() && (matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state.is_pressed() && event.logical_key == Key::Named(NamedKey::Escape))
            || matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. })) {
            self.cancel();
            return true;
        }
        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => self.press(point, move_allowed),
            WindowEvent::CursorMoved { .. } => self.motion(point),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if self.drag.is_some() => {
                self.motion(point);
                self.drag = None;
                true
            }
            WindowEvent::MouseWheel { .. } | WindowEvent::PinchGesture { .. } if self.drag.is_some() => true,
            _ => false,
        }
    }

    pub(crate) fn paint_grip(&self, g: &mut gpu::GpuRenderer, point: Point) {
        let Some((x, y, w, h)) = self.rect else { return };
        let color = if self.edges(point) != 0 { theme::accent() } else { theme::text_mute() };
        for n in 1..=3 {
            let width = n as f32 * 3.0;
            g.rect(x + w - 4.0 - width, y + h - 4.0 - (3 - n) as f32 * 3.0, width, 1.0, color);
        }
    }
}

impl App {
    pub(crate) fn preview_frame_probe(&self) {
        if !crate::verification_run() { return; }
        let Some(path) = std::env::var_os("KASATERM_TEST_PREVIEW_FRAME_REPORT") else { return };
        let data = self.image_preview.frame_probe().or_else(|| self.media_preview.frame_probe());
        static LAST: std::sync::OnceLock<std::sync::Mutex<String>> = std::sync::OnceLock::new();
        let Ok(mut last) = LAST.get_or_init(Default::default).lock() else { return };
        let text = serde_json::to_string(&data).unwrap_or_default();
        if *last != text {
            let _ = std::fs::write(path, &text);
            *last = text;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preview_uses_a_quarter_of_the_application() {
        let mut frame = Frame::default();
        let rect = frame.layout((1200.0, 800.0), None, (320.0, 240.0));
        assert_eq!(rect, (300.0, 200.0, 600.0, 400.0));
    }

    #[test]
    fn moving_and_resizing_keep_the_frame_on_screen_and_cancel_restores_it() {
        let mut frame = Frame::default();
        let original = frame.layout((1200.0, 800.0), None, (320.0, 240.0));
        assert!(frame.press((400.0, 224.0), true));
        frame.motion((500.0, 324.0));
        assert_eq!(frame.rect(), Some((400.0, 300.0, 600.0, 400.0)));
        frame.cancel();
        assert_eq!(frame.rect(), Some(original));
        assert!(frame.press((899.0, 599.0), false));
        frame.motion((999.0, 659.0));
        assert_eq!(frame.rect(), Some((300.0, 200.0, 700.0, 460.0)));
        frame.drag = None;
        let rect = frame.layout((500.0, 320.0), None, (320.0, 240.0));
        assert!(rect.0 >= 0.0 && rect.1 >= 0.0 && rect.0 + rect.2 <= 500.0 && rect.1 + rect.3 <= 320.0);
    }

    #[test]
    fn shrinking_a_corner_stops_at_minimum_dimensions() {
        let mut frame = Frame::default();
        frame.layout((1200.0, 800.0), None, (320.0, 240.0));
        assert!(frame.press((300.0, 200.0), false));
        frame.motion((1100.0, 750.0));
        assert_eq!(frame.rect(), Some((580.0, 360.0, 320.0, 240.0)));
        assert_eq!(frame.cursor((600.0, 400.0), false), Some(CursorIcon::NwseResize));
    }
}
