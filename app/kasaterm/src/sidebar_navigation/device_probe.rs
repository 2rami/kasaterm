use super::*;

const A: &str = "검증 A";
const B: &str = "검증 B";

fn fixture(label: &str, count: usize) -> state::MachinesColMachine {
    state::MachinesColMachine {
        label: label.into(), online: true, ago_secs: Some(0), outdated: false,
        host: String::new(), kvm: None, mirrored: Vec::new(), closed: 0, web_shells: Vec::new(), link: Default::default(),
        remote: (0..count).map(|n| state::MachinesColRow {
            pane: String::new(), remote_id: format!("%device-probe-{n}"), remote_cwd: "/workspace/probe".into(),
            name: String::new(), title: format!("작업 {}", n + 1), status: "idle".into(),
            room: format!("검증 방 {}", n + 1), closed: false, rect: Some([0.0, 0.0, 1.0, 1.0]), window: Some(n as u64),
            tab_of: None, attention_kind: None,
        }).collect(),
    }
}

impl App {
    pub(super) fn run_pending_sidebar_device_probe(&mut self, event_loop: &ActiveEventLoop) {
        if !crate::verification_run() || !std::env::var("KASATERM_MACHINES").is_ok_and(|s| s == "[]") { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTODEVICEORDER_DIR") else { return };
        static STEP: std::sync::OnceLock<std::sync::Mutex<(Instant, usize, usize)>> = std::sync::OnceLock::new();
        let mut step = STEP.get_or_init(|| std::sync::Mutex::new((Instant::now(), 0, 0))).lock().unwrap();
        let restored = std::env::var_os("KASATERM_DEVICEORDER_RESTORE").is_some();
        self.info.machines_col.machines = if step.1 < 3 { vec![fixture(A, 1), fixture(B, 8)] }
            else { vec![fixture(B, 8), fixture(A, 1)] };
        self.info.machines_col.last_refresh = Some(Instant::now());
        if step.1 >= 11 || step.0.elapsed().as_millis() < if step.1 == 0 { 6500 } else { 800 } { return; }
        let Some(window) = self.window.as_ref().map(|w| w.id()) else { return };
        let root = std::path::Path::new(&folder);
        let _ = std::fs::create_dir_all(root);
        let move_to = |app: &mut Self, point: (f32, f32)| {
            let scale = app.effective_scale();
            app.window_event(event_loop, window, WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new((point.0 * scale) as f64, (point.1 * scale) as f64),
            });
        };
        let button = |app: &mut Self, state| app.window_event(event_loop, window, WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(), state, button: MouseButton::Left,
        });
        let head = |app: &Self, label: &str| app.info.navigation.device_heads.iter()
            .find(|(key, _)| key == label).map(|(_, r)| (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0));
        let click = |app: &mut Self, label: &str| {
            if let Some(point) = head(app, label) {
                move_to(app, point);
                button(app, ElementState::Pressed);
                button(app, ElementState::Released);
                app.chrome_dirty = true;
                app.render_frame();
                true
            } else { false }
        };
        let order = |app: &Self| app.info.navigation.device_sections.iter().map(|s| s.key.clone()).collect::<Vec<_>>();
        let mut checks = serde_json::Map::new();
        let mut check = |name: &str, ok: bool| { checks.insert(name.to_string(), serde_json::json!(ok)); };
        let stage = step.1;
        if stage == 0 {
            self.sidebar_visible = true;
            self.sidebar_list_body = std::env::var_os("KASATERM_DEVICEORDER_LIST").is_some();
            self.info.navigation.list_rooms.default_list = self.sidebar_list_body;
            if let Some(width) = std::env::var("KASATERM_DEVICEORDER_WIDTH").ok().and_then(|s| s.parse::<f32>().ok()) {
                self.sidebar_w_logical = width.clamp(124.0, 520.0);
            }
            self.chrome_dirty = true;
            self.render_frame();
            step.2 = self.windows.len();
            if restored {
                check("restored_order", order(self) == [B, A, ""]);
                check("restored_device_collapse", ["", A, B].iter().all(|key| self.info.navigation.collapsed_devices.contains(*key)));
                check("restored_no_local_rooms", self.window_tab_rects.is_empty());
                check("restored_no_remote_rooms", !self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { .. })));
                step.1 = 10;
            } else {
                check("initial_order", order(self) == ["", A, B]);
                check("initial_expanded", self.info.navigation.collapsed_devices.is_empty());
                check("initial_local_rooms_visible", !self.window_tab_rects.is_empty());
                check("initial_remote_rooms_visible", self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { .. })));
            }
        } else if stage == 1 {
            let source = head(self, B);
            check("source_header_visible", source.is_some());
            if let Some(source) = source {
                move_to(self, source);
                button(self, ElementState::Pressed);
                move_to(self, (source.0, self.sidebar_content_top() + 2.0));
                check("drag_captured", self.info.navigation.device_drag.as_ref().is_some_and(|d| d.active));
                check("order_not_saved_before_release", socket::read_settings()["sidebar_device_order"].as_array().is_none_or(|keys| keys.is_empty()));
            }
        } else if stage == 2 {
            button(self, ElementState::Released);
            self.chrome_dirty = true;
            self.render_frame();
            check("remote_before_local", order(self) == [B, "", A]);
            check("remote_collapsed_by_header", click(self, B) && self.info.navigation.collapsed_devices.contains(B));
            check("local_collapsed_by_header", click(self, "") && self.sidebar_local_device_collapsed());
            check("local_room_hits_hidden", self.window_tab_rects.is_empty());
            check("other_device_collapsed", click(self, A) && self.info.navigation.collapsed_devices.contains(A));
            check("remote_room_hits_hidden", !self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { .. })));
        } else if stage == 3 {
            self.chrome_dirty = true;
            self.render_frame();
            check("polling_keeps_order", order(self) == [B, "", A]);
            if let Some(source) = head(self, "") {
                move_to(self, source);
                button(self, ElementState::Pressed);
                let bottom = self.info.navigation.device_sections.last().map_or(source.1, |s| s.top + s.height);
                move_to(self, (source.0, bottom + 12.0));
                button(self, ElementState::Released);
                self.chrome_dirty = true;
                self.render_frame();
            }
            check("collapsed_local_moves_to_end", order(self) == [B, A, ""]);
        } else if stage == 4 {
            let source = head(self, A);
            check("outside_source_visible", source.is_some());
            if let Some(source) = source {
                move_to(self, source);
                button(self, ElementState::Pressed);
                move_to(self, (self.tab_strip_w() + 60.0, source.1 + 20.0));
                button(self, ElementState::Released);
            }
            check("outside_drop_cancels", order(self) == [B, A, ""] && self.info.navigation.device_drag.is_none());
        } else if stage == 5 {
            let source = head(self, B);
            check("focus_source_visible", source.is_some());
            if let Some(source) = source {
                move_to(self, source);
                button(self, ElementState::Pressed);
                move_to(self, (source.0, source.1 + 20.0));
                check("cancel_drag_captured", self.info.navigation.device_drag.as_ref().is_some_and(|d| d.active));
                self.window_event(event_loop, window, WindowEvent::Focused(false));
                self.window_event(event_loop, window, WindowEvent::Focused(true));
                button(self, ElementState::Released);
            }
            check("focus_loss_cancels", self.info.navigation.device_drag.is_none() && order(self) == [B, A, ""]);
        } else if stage == 6 {
            check("expand_keeps_rooms", click(self, B) && !self.info.navigation.collapsed_devices.contains(B)
                && self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { label, .. } if label == B)));
            check("scroll_starts_at_top", self.sidebar_scroll_px == 0.0);
            let source = head(self, B);
            check("scroll_source_visible", source.is_some());
            if let Some(source) = source {
                move_to(self, source);
                button(self, ElementState::Pressed);
                if let Some(view) = self.info.navigation.device_viewport {
                    move_to(self, (view.2 / 2.0, view.1 + view.3 - 2.0));
                }
            }
        } else if stage == 7 {
            check("stationary_edge_scroll", self.sidebar_scroll_px > 20.0 && self.info.navigation.device_drag.as_ref().is_some_and(|d| d.active));
            self.window_event(event_loop, window, WindowEvent::Focused(false));
            self.window_event(event_loop, window, WindowEvent::Focused(true));
            button(self, ElementState::Released);
            self.sidebar_scroll_px = 0.0;
            self.chrome_dirty = true;
            self.render_frame();
            check("collapse_after_scroll", click(self, B) && self.info.navigation.collapsed_devices.contains(B));
        } else if stage == 8 {
            let settings = socket::read_settings();
            let loaded = NavigationState::from_settings(&settings);
            check("saved_order", loaded.device_order.keys == [B, A, ""]);
            check("saved_collapse", ["", A, B].iter().all(|key| loaded.collapsed_devices.contains(*key)));
            check("rooms_not_moved", self.windows.len() == step.2);
            check("no_remote_sessions_created", self.pty.keys().all(|id| !kasa_mcp::remote::is_remote_pane(id)));
            step.1 = 10;
        }
        let _ = std::fs::write(root.join(format!("{stage:02}.json")), serde_json::json!({"checks": checks, "order": order(self)}).to_string());
        if matches!(stage, 0 | 1 | 2 | 3 | 8) {
            if stage != 1 { move_to(self, (self.tab_strip_w() + 40.0, self.sidebar_content_top() + 20.0)); }
            self.chrome_dirty = true;
            if let Some(g) = &mut self.gpu { g.capture_next = Some(root.join(format!("{stage:02}.png")).to_string_lossy().into_owned()); }
            self.render_frame();
        }
        step.0 = Instant::now();
        step.1 += 1;
    }
}
