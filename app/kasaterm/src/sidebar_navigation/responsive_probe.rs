use super::*;

impl App {
    pub(super) fn run_pending_sidebar_responsive_probe(&mut self, event_loop: &ActiveEventLoop) {
        if !crate::verification_run() { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTOSIDEBAR_DIR") else { return; };
        static STEP: std::sync::OnceLock<std::sync::Mutex<(Instant, usize)>> = std::sync::OnceLock::new();
        let mut step = STEP.get_or_init(|| std::sync::Mutex::new((Instant::now(), 0))).lock().unwrap();
        let widths = [140.0, 240.0, 360.0, 520.0, 140.0];
        if step.1 >= widths.len() * 4 || step.0.elapsed().as_millis() < if step.1 == 0 { 6500 } else { 700 } { return; }
        let Some(window) = self.window.as_ref().map(|w| w.id()) else { return; };
        let index = step.1 / 2;
        let width = widths[index % widths.len()];
        let listed = index < widths.len();
        let mode = if listed { "list" } else { "map" };
        if std::env::var_os("KASATERM_AUTOSIDEBAR_EXTRAS").is_some() {
            self.info.machines_col.local_web_shells = vec![
                state::WebShell { id: "sidebar-fixture-idle".into(), cwd: "/workspace/sidebar-review".into(), job: None },
                state::WebShell { id: "sidebar-fixture-busy".into(), cwd: "/workspace/sidebar-tests".into(), job: Some("cargo test".into()) },
            ];
            self.info.navigation.web_open.insert(String::new());
        }
        if step.1 % 2 == 0 {
            if step.1 == 0 && std::env::var_os("KASATERM_AUTOSIDEBAR_EXTRAS").is_some() {
                let ids = self.window_leaves(self.active_window);
                if ids.len() >= 4 {
                    self.stash_pane(&ids[ids.len() - 1]);
                    self.undock_pane_terminal(&ids[ids.len() - 2], event_loop, None);
                    self.undock_pane_terminal(&ids[ids.len() - 3], event_loop, None);
                }
            }
            self.sidebar_visible = true;
            self.sidebar_list_body = listed;
            self.info.navigation.list_rooms.default_list = listed;
            self.cursor_px = (self.tab_strip_w(), TITLE_HEIGHT + 100.0);
            self.window_event(event_loop, window, WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(), state: ElementState::Pressed, button: MouseButton::Left,
            });
            self.window_event(event_loop, window, WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new((width * self.effective_scale()) as f64,
                    ((TITLE_HEIGHT + 100.0) * self.effective_scale()) as f64),
            });
            self.window_event(event_loop, window, WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(), state: ElementState::Released, button: MouseButton::Left,
            });
            self.cursor_px = (width + 100.0, TITLE_HEIGHT + 100.0);
            self.chrome_dirty = true;
            if let Some(w) = &self.window { w.request_redraw(); }
        } else {
            let height = self.window.as_ref().unwrap().inner_size().height as f32 / self.effective_scale();
            let (cards, _, _, rows, maps, detached) = self.sidebar_layout(height);
            let view = (0.0, self.sidebar_content_top(), self.tab_strip_w(), self.sidebar_avail_h(height));
            let remote_hits: Vec<_> = self.info.navigation.hits.iter().map(|(_, r)| *r).collect();
            let hits_within = remote_hits.iter().all(|r| r.0 >= 0.0 && r.0 + r.2 <= self.tab_strip_w() + 0.5);
            let candidate = if listed { rows.first() } else { maps.first() }.cloned();
            let mut clicked = false;
            if let Some((_, id, r)) = candidate {
                if let Some(r) = clipped(r, view) {
                    self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    for state in [ElementState::Pressed, ElementState::Released] {
                        self.window_event(event_loop, window, WindowEvent::MouseInput {
                            device_id: winit::event::DeviceId::dummy(), state, button: MouseButton::Left,
                        });
                    }
                    clicked = self.ws.lock().unwrap().active_pane.as_deref() == Some(id.as_str());
                }
            }
            let root = std::path::Path::new(&folder);
            let _ = std::fs::create_dir_all(root);
            let stem = format!("{index:02}-{mode}-{width:.0}");
            self.cursor_px = (width / 2.0, view.1 + 20.0);
            self.handle_wheel(MouseScrollDelta::LineDelta(0.0, -1000.0));
            let scroll_reached_end = (self.sidebar_scroll_px - self.sidebar_max_scroll(height)).abs() < 0.5;
            self.chrome_dirty = true;
            if self.sidebar_max_scroll(height) > 0.0 {
                if let Some(g) = self.gpu.as_mut() { g.capture_next = Some(root.join(format!("{stem}-bottom.png")).to_string_lossy().into_owned()); }
            }
            self.render_frame();
            let scrolled_hits_within = self.info.navigation.hits.iter().all(|(action, r)| {
                matches!(action, Action::NewLocalRoom) || (r.1 >= view.1 && r.1 + r.3 <= view.1 + view.3 + 0.5)
            });
            self.handle_wheel(MouseScrollDelta::LineDelta(0.0, 1000.0));
            let report = serde_json::json!({"mode": mode, "expected_width": width, "width": self.tab_strip_w(),
                "scale": self.effective_scale(), "cards": cards, "rows": rows, "maps": maps, "detached": detached,
                "remote_hits": remote_hits, "hits_within_width": hits_within, "clicked_expected_pane": clicked,
                "scroll_reached_end": scroll_reached_end, "scrolled_hits_within": scrolled_hits_within,
                "content_h": self.sidebar_content_h(), "max_scroll": self.sidebar_max_scroll(height), "view": view});
            let _ = std::fs::write(root.join(format!("{stem}.json")), report.to_string());
            self.cursor_px = (width + 100.0, TITLE_HEIGHT + 100.0);
            self.chrome_dirty = true;
            if let Some(g) = self.gpu.as_mut() { g.capture_next = Some(root.join(format!("{stem}.png")).to_string_lossy().into_owned()); }
            self.render_frame();
        }
        step.0 = Instant::now();
        step.1 += 1;
    }
}
