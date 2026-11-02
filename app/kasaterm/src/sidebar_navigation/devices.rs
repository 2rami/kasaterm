use super::*;
use super::device_order::{Order, Section};

pub(super) const LOCAL_GAP: f32 = 10.0;

pub(super) struct DeviceDrag {
    pub(super) label: String,
    pub(super) start: (f32, f32),
    pub(super) active: bool,
    scroll_at: Instant,
}

impl DeviceDrag {
    pub(super) fn new(label: String, start: (f32, f32)) -> Self {
        Self { label, start, active: false, scroll_at: Instant::now() }
    }
}

impl NavigationState {
    pub(crate) fn from_settings(settings: &serde_json::Value) -> Self {
        let keys = |name: &str| settings[name].as_array().into_iter().flatten()
            .filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>();
        Self {
            device_order: Order::new(keys("sidebar_device_order")),
            collapsed_devices: keys("sidebar_collapsed_devices").into_iter().collect(),
            ..Default::default()
        }
    }
}

pub(super) fn sections(info: &state::InfoState, width: f32, local_content_h: f32) -> Vec<Section> {
    let nav = &info.navigation;
    let local_body = if nav.collapsed_devices.contains("") { 0.0 } else {
        LOCAL_GAP + local_content_h + web_block_h(&info.machines_col.local_web_shells, nav.web_open.contains(""), width)
    };
    let mut sizes = vec![(String::new(), HEADER_H + local_body)];
    sizes.extend(info.machines_col.machines.iter().map(|machine| {
        let body = if nav.collapsed_devices.contains(&machine.label) { 0.0 } else {
            content_height(machine, &nav.collapsed_rooms, &nav.list_rooms, width)
                + web_block_h(&machine.web_shells, nav.web_open.contains(&machine.label), width)
        };
        (machine.label.clone(), SECTION_H + body)
    }));
    device_order::layout(&nav.device_order, &sizes)
}

fn drop_at(nav: &NavigationState, cursor: (f32, f32)) -> Option<(Order, f32)> {
    let drag = nav.device_drag.as_ref().filter(|drag| drag.active)?;
    let viewport = nav.device_viewport?;
    if !hit(cursor, viewport) { return None; }
    let (target, side, y) = device_order::insertion(&nav.device_sections, &drag.label, cursor.1)?;
    let live = nav.device_sections.iter().map(|section| section.key.clone()).collect::<Vec<_>>();
    let order = nav.device_order.moved(&live, &drag.label, &target, side)?;
    Some((order, y.clamp(viewport.1 + 2.0, viewport.1 + viewport.3 - 2.0)))
}

pub(super) fn draw_drag(g: &mut gpu::GpuRenderer, info: &state::InfoState, cursor: (f32, f32)) {
    let nav = &info.navigation;
    let Some(drag) = nav.device_drag.as_ref().filter(|drag| drag.active) else { return };
    let Some(view) = nav.device_viewport else { return };
    g.push_clip(view.0, view.1, view.2, view.3);
    if let Some(section) = nav.device_sections.iter().find(|section| section.key == drag.label) {
        g.round_rect_stroke(4.0, section.top + 2.0, (view.2 - 8.0).max(0.0), (section.height - 4.0).max(0.0),
            12.0, 2.0, theme::accent());
    }
    if let Some((_, y)) = drop_at(nav, cursor) {
        g.rect(8.0, y - 1.5, (view.2 - 16.0).max(0.0), 3.0, theme::accent());
    }
    let w = (view.2 - 24.0).clamp(40.0, 220.0);
    let x = (cursor.0 + 10.0).clamp(8.0, (view.2 - w - 8.0).max(8.0));
    let y = (cursor.1 + 12.0).clamp(view.1 + 4.0, (view.1 + view.3 - 36.0).max(view.1 + 4.0));
    g.round_rect_fill(x, y, w, 32.0, 6.0, theme::surface_active());
    g.round_rect_stroke(x, y, w, 32.0, 6.0, 1.0, theme::accent());
    let name = if drag.label.is_empty() {
        crate::render::pane_identity::local_device_name().unwrap_or_else(|| "이 기기".into())
    } else { crate::render::pane_identity::device_name(&drag.label) };
    text(g, &name, x + 10.0, y + 8.0, w - 20.0, 12.0, theme::text(), true);
    g.pop_clip();
}

impl App {
    pub(crate) fn sidebar_devices_content_h(&self) -> f32 {
        sections(&self.info, self.tab_strip_w(), self.sidebar_local_content_h())
            .last().map_or(0.0, |section| section.top + section.height)
    }

    pub(crate) fn sidebar_local_device_top(&self) -> f32 {
        sections(&self.info, self.tab_strip_w(), self.sidebar_local_content_h())
            .into_iter().find(|section| section.key.is_empty()).map_or(0.0, |section| section.top)
    }

    pub(crate) fn sidebar_local_body_offset(&self) -> f32 {
        self.sidebar_local_device_top() + HEADER_H + LOCAL_GAP
    }

    pub(crate) fn sidebar_local_device_collapsed(&self) -> bool {
        self.info.navigation.collapsed_devices.contains("")
    }

    pub(crate) fn sidebar_device_header_at(&self, cursor: (f32, f32)) -> bool {
        !self.tabs_on_top && self.sidebar_visible && self.info.navigation.device_heads.iter().any(|(_, rect)| hit(cursor, *rect))
    }

    pub(crate) fn sidebar_device_drag_event(&mut self, event: &WindowEvent) -> bool {
        let escape = matches!(event, WindowEvent::KeyboardInput { event, .. }
            if event.state == ElementState::Pressed && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape));
        if (escape || matches!(event, WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. }))
            && self.info.navigation.device_drag.take().is_some() {
            self.chrome_dirty = true;
            if let Some(window) = &self.window {
                window.set_cursor(CursorIcon::Default);
                window.request_redraw();
            }
            return escape;
        }
        false
    }

    pub(super) fn sidebar_device_release(&mut self, cursor: (f32, f32)) -> bool {
        let next = drop_at(&self.info.navigation, cursor);
        let Some(drag) = self.info.navigation.device_drag.take() else { return false };
        if drag.active {
            if let Some((order, _)) = next {
                socket::write_setting("sidebar_device_order", serde_json::json!(order.keys));
                self.info.navigation.device_order = order;
            }
        } else if self.info.navigation.device_heads.iter().any(|(label, rect)| *label == drag.label && hit(cursor, *rect)) {
            let collapsed = &mut self.info.navigation.collapsed_devices;
            if !collapsed.remove(&drag.label) { collapsed.insert(drag.label); }
            let mut keys: Vec<_> = collapsed.iter().cloned().collect();
            keys.sort();
            socket::write_setting("sidebar_collapsed_devices", serde_json::json!(keys));
        }
        self.close_freeze.thaw();
        if let Some(window) = &self.window {
            let height = window.inner_size().height as f32 / self.effective_scale();
            self.sidebar_scroll_px = self.sidebar_scroll_px.clamp(0.0, self.sidebar_max_scroll(height));
            window.set_cursor(if self.sidebar_device_header_at(cursor) { CursorIcon::Grab } else { CursorIcon::Default });
        }
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn sidebar_device_drag_deadline(&self) -> Option<Instant> {
        let nav = &self.info.navigation;
        let drag = nav.device_drag.as_ref().filter(|drag| drag.active)?;
        let speed = device_order::scroll_speed(self.cursor_px, nav.device_viewport?);
        let height = self.window.as_ref()?.inner_size().height as f32 / self.effective_scale();
        let can_scroll = (speed < 0.0 && self.sidebar_scroll_px > 0.0)
            || (speed > 0.0 && self.sidebar_scroll_px < self.sidebar_max_scroll(height));
        can_scroll.then_some(drag.scroll_at + std::time::Duration::from_millis(16))
    }

    pub(crate) fn tick_sidebar_device_drag(&mut self) {
        let Some(view) = self.info.navigation.device_viewport else { return };
        let speed = device_order::scroll_speed(self.cursor_px, view);
        let now = Instant::now();
        let Some(drag) = self.info.navigation.device_drag.as_mut().filter(|drag| drag.active) else { return };
        let elapsed = now.duration_since(drag.scroll_at).as_secs_f32().min(0.05);
        drag.scroll_at = now;
        if speed == 0.0 { return; }
        let Some(window) = &self.window else { return };
        let height = window.inner_size().height as f32 / self.effective_scale();
        let next = (self.sidebar_scroll_px + speed * elapsed).clamp(0.0, self.sidebar_max_scroll(height));
        if next != self.sidebar_scroll_px {
            self.sidebar_scroll_px = next;
            self.chrome_dirty = true;
            window.request_redraw();
        }
    }
}
