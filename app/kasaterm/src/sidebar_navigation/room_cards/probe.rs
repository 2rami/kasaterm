use super::*;

const PEER: &str = "검증 기기";
#[derive(PartialEq)]
enum Finish { Commit, Cancel }

impl App {
    pub(crate) fn run_pending_sidebar_room_cards_probe(&mut self, event_loop: &ActiveEventLoop) {
        if !crate::verification_run() || !std::env::var("KASATERM_MACHINES").is_ok_and(|s| s == "[]") { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTOROOMCARDS_DIR") else { return };
        static STEP: std::sync::OnceLock<std::sync::Mutex<(Instant, usize, Vec<(String, Option<u32>)>)>> = std::sync::OnceLock::new();
        let mut step = STEP.get_or_init(|| std::sync::Mutex::new((Instant::now(), 0, Vec::new()))).lock().unwrap();
        self.info.machines_col.machines = vec![super::super::device_probe::fixture(PEER, 2)];
        self.info.machines_col.last_refresh = Some(Instant::now());
        if step.1 > 8 || step.0.elapsed().as_millis() < if step.1 == 0 { 6500 } else { 800 } { return; }
        let Some(window) = self.window.as_ref().map(|w| w.id()) else { return };
        let restored = std::env::var_os("KASATERM_ROOMCARDS_RESTORE").is_some();
        if restored && self.restore_applying.is_some() { return; }
        let processes = |app: &Self| {
            let mut ids: Vec<_> = app.pty.iter().map(|(id, p)| (id.clone(), p.shell_pid())).collect();
            ids.sort();
            ids
        };
        let root = std::path::Path::new(&folder);
        let _ = std::fs::create_dir_all(root);
        let move_to = |app: &mut Self, point: (f32, f32)| {
            let scale = app.effective_scale();
            app.window_event(event_loop, window, WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new((point.0 * scale) as f64, (point.1 * scale) as f64),
            });
        };
        let button = |app: &mut Self, state, button| app.window_event(event_loop, window, WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(), state, button,
        });
        let click = |app: &mut Self, rect: Rect, which| {
            move_to(app, (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0));
            button(app, ElementState::Pressed, which);
            button(app, ElementState::Released, which);
            app.chrome_dirty = true;
            app.render_frame();
        };
        let resize = |app: &mut Self, room: &RoomId, height: f32, finish| {
            let grip = app.info.navigation.room_cards.grips.iter().find(|g| &g.id == room).cloned();
            let Some(grip) = grip else { return false };
            let point = (grip.rect.0 + grip.rect.2 / 2.0, grip.rect.1 + grip.rect.3 / 2.0);
            move_to(app, point);
            button(app, ElementState::Pressed, MouseButton::Left);
            move_to(app, (point.0, point.1 + height - grip.height));
            if finish == Finish::Cancel {
                app.window_event(event_loop, window, WindowEvent::Focused(false));
                app.window_event(event_loop, window, WindowEvent::Focused(true));
            }
            button(app, ElementState::Released, MouseButton::Left);
            app.chrome_dirty = true;
            app.render_frame();
            true
        };
        let local_menu = |app: &mut Self, room, action| {
            let rect = app.window_tab_rects.iter().find(|(i, _)| *i == room).map(|(_, r)| *r);
            let Some(rect) = rect else { return false };
            click(app, (rect.0 + 24.0, rect.1 + 12.0, 2.0, 2.0), MouseButton::Right);
            let item = app.sidebar_menu_rects.iter().find(|(a, _)| *a == action).map(|(_, r)| *r);
            if let Some(rect) = item { click(app, rect, MouseButton::Left); true } else { false }
        };
        let remote0 = RoomId::Remote(room_key(PEER, Some(0), ""));
        let remote1 = RoomId::Remote(room_key(PEER, Some(1), ""));
        let mut checks = serde_json::Map::new();
        let mut check = |name: &str, value: bool| { checks.insert(name.into(), serde_json::json!(value)); };
        let stage = step.1;
        match stage {
            0 => {
                self.sidebar_visible = true;
                self.sidebar_w_logical = 320.0;
                self.sidebar_list_body = false;
                if !restored && self.windows.len() == 1 { self.new_window(); }
                if !restored {
                    self.window_name_override.insert(0, "높이 검증".into());
                    self.window_name_override.insert(1, "숨김 검증".into());
                }
                self.window_labels_at = None;
                self.chrome_dirty = true;
                self.render_frame();
                check("two_live_rooms", self.windows.len() == 2 && self.pty.len() == 2);
                step.2 = processes(self);
                check("live_shell_processes", step.2.iter().all(|(_, pid)| pid.is_some()));
                if restored {
                    check("restored_local_height", self.sidebar_room_height(0, 10.0, false) == 230.0);
                    check("restored_remote_height", self.info.navigation.room_cards.height(&remote0, 10.0, false) == 180.0);
                    check("restored_local_hidden", self.sidebar_room_hidden(1));
                    check("restored_remote_hidden", self.info.navigation.room_cards.hidden(&remote1));
                    check("hidden_local_has_no_hit", !self.window_tab_rects.iter().any(|(i, _)| *i == 1));
                    step.1 = 8;
                }
            }
            1 => {
                let active = self.active_window;
                check("local_grip", resize(self, &RoomId::Local(0), 230.0, Finish::Commit));
                check("local_height_changed", self.sidebar_room_height(0, 10.0, false) == 230.0);
                check("resize_does_not_switch_room", self.active_window == active);
            }
            2 => {
                check("cancel_grip", resize(self, &RoomId::Local(0), 300.0, Finish::Cancel));
                check("cancel_keeps_height", self.sidebar_room_height(0, 10.0, false) == 230.0);
            }
            3 => {
                check("remote_grip", resize(self, &remote0, 180.0, Finish::Commit));
                check("remote_height_changed", self.info.navigation.room_cards.height(&remote0, 10.0, false) == 180.0);
            }
            4 => {
                check("local_hide_menu", local_menu(self, 1, SidebarMenuAction::HideRoom));
                check("local_hidden", self.sidebar_room_hidden(1));
                check("local_hit_removed", !self.window_tab_rects.iter().any(|(i, _)| *i == 1));
                check("local_processes_alive", processes(self) == step.2 && self.windows.len() == 2);
            }
            5 => {
                move_to(self, (self.tab_strip_w() / 2.0, self.sidebar_content_top() + 100.0));
                self.handle_wheel(MouseScrollDelta::LineDelta(0.0, -1000.0));
                self.chrome_dirty = true;
                self.render_frame();
                let head = self.info.navigation.hits.iter().find_map(|(a, r)|
                    matches!(a, Action::Open { label, window: Some(1), focus: None, .. } if label == PEER).then_some(*r));
                check("remote_header_visible", head.is_some());
                if let Some(rect) = head {
                    click(self, (rect.0 + 24.0, rect.1 + 12.0, 2.0, 2.0), MouseButton::Right);
                    let item = self.info.navigation.room_menu_rects.iter().find_map(|(a, r)| (*a == RoomMenuAction::Hide).then_some(*r));
                    check("remote_hide_menu", item.is_some());
                    if let Some(rect) = item { click(self, rect, MouseButton::Left); }
                }
                check("remote_hidden", self.info.navigation.room_cards.hidden(&remote1));
                check("remote_hit_removed", !self.info.navigation.hits.iter().any(|(a, _)| matches!(a, Action::Open { window: Some(1), .. })));
                check("remote_rooms_preserved", self.info.machines_col.machines[0].remote.len() == 2);
            }
            6 => {
                self.sidebar_scroll_px = 0.0;
                self.chrome_dirty = true;
                self.render_frame();
                let head = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::HiddenRooms(label) if label.is_empty()).then_some(*r));
                check("hidden_rooms_entry", head.is_some());
                if let Some(rect) = head { click(self, rect, MouseButton::Left); }
                let row = self.info.navigation.hits.iter().find_map(|(a, r)| matches!(a, Action::ShowHiddenRoom(RoomId::Local(1))).then_some(*r));
                check("restore_entry", row.is_some());
                if let Some(rect) = row { click(self, rect, MouseButton::Left); }
                check("restored_without_restart", !self.sidebar_room_hidden(1) && processes(self) == step.2);
            }
            7 => {
                check("hide_again", local_menu(self, 1, SidebarMenuAction::HideRoom));
                check("hidden_again", self.sidebar_room_hidden(1));
                check("reset_size_menu", local_menu(self, 0, SidebarMenuAction::ResetRoomSize));
                check("reset_uses_natural_height", !self.info.navigation.room_cards.prefs.local_heights.contains_key(&0));
                check("resize_after_reset", resize(self, &RoomId::Local(0), 230.0, Finish::Commit));
            }
            8 => {
                let saved = self.session_state_json().unwrap_or_default();
                let p = &saved["sessions"][self.active_session]["sidebar_room_cards"];
                check("saved_local_height", p["local_heights"]["0"].as_f64() == Some(230.0));
                check("saved_local_hidden", p["local_hidden"].as_array().is_some_and(|s| s.iter().any(|v| v.as_u64() == Some(1))));
                check("saved_remote_hidden", p["remote_hidden"].as_array().is_some_and(|s| s.iter().any(|v| v.as_str() == Some(room_key(PEER, Some(1), "").as_str()))));
                check("all_processes_preserved", processes(self) == step.2);
            }
            _ => {}
        }
        let _ = std::fs::write(root.join(format!("{stage:02}.json")), serde_json::json!({"checks":checks}).to_string());
        move_to(self, (self.tab_strip_w() + 40.0, self.sidebar_content_top() + 20.0));
        self.chrome_dirty = true;
        if let Some(g) = &mut self.gpu { g.capture_next = Some(root.join(format!("{stage:02}.png")).to_string_lossy().into_owned()); }
        self.render_frame();
        step.0 = Instant::now();
        step.1 += 1;
    }
}
