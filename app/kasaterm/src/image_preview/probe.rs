use super::*;
use winit::event::{DeviceId, TouchPhase};

#[derive(Default)]
pub(super) struct Probe {
    next: Option<Instant>,
    step: usize,
    records: Vec<serde_json::Value>,
    owner: Option<WindowId>,
    done: bool,
}

impl App {
    pub(crate) fn image_preview_probe(&mut self, event_loop: &ActiveEventLoop) {
        if !crate::verification_run() {
            return;
        }
        let Some(report) = std::env::var_os("KASATERM_TEST_IMAGE_PREVIEW") else {
            return;
        };
        let mut probe = std::mem::take(&mut self.image_preview.probe);
        if probe.done {
            self.image_preview.probe = probe;
            return;
        }
        let wait = |proxy: EventLoopProxy<UserEvent>, millis| {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(millis));
                let _ = proxy.send_event(UserEvent::Redraw);
            });
        };
        if probe.next.is_none() {
            probe.next = Some(Instant::now() + std::time::Duration::from_secs(6));
            wait(self.proxy.clone(), 6100);
            self.image_preview.probe = probe;
            return;
        }
        if probe.next.is_some_and(|at| Instant::now() < at) {
            self.image_preview.probe = probe;
            return;
        }
        let actions = std::env::var("KASATERM_TEST_IMAGE_PREVIEW_ACTIONS").unwrap_or_else(|_| {
            "drag;click;zoom_in;original;pan_far;fit;copy;save;ime;escape".into()
        });
        let actions: Vec<_> = actions.split(';').collect();
        let aux = std::env::var_os("KASATERM_TEST_IMAGE_PREVIEW_AUX").is_some();
        let owner = probe.owner.or_else(|| {
            if aux {
                self.aux.terminals.first().map(|t| t.window.id())
            } else {
                self.window.as_ref().map(|w| w.id())
            }
        });
        let Some(owner) = owner else {
            self.image_preview.probe = probe;
            return;
        };
        probe.owner = Some(owner);
        let hits: Vec<_> = self
            .image_preview
            .hits
            .get(&owner)
            .map(|(_, h)| {
                h.iter()
                    .map(|h| serde_json::json!({"key":h.key,"path":h.path,"rects":h.rects}))
                    .collect()
            })
            .unwrap_or_default();
        let explicit_point = std::env::var("KASATERM_TEST_IMAGE_PREVIEW_POINT")
            .ok()
            .and_then(|s| {
                let (x, y) = s.split_once(',')?;
                Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?))
            });
        if hits.is_empty() && probe.step == 0 && explicit_point.is_none() {
            probe.next = Some(Instant::now() + std::time::Duration::from_millis(500));
            wait(self.proxy.clone(), 550);
            self.image_preview.probe = probe;
            return;
        }
        if self
            .image_preview
            .preview
            .as_ref()
            .is_some_and(|p| p.pixels.is_none() && p.status == "그림을 불러오는 중…" || p.busy)
        {
            probe.next = Some(Instant::now() + std::time::Duration::from_millis(500));
            wait(self.proxy.clone(), 550);
            self.image_preview.probe = probe;
            return;
        }
        let modal = self.image_preview.preview.as_ref().map(|p| serde_json::json!({
            "zoom": p.zoom,"pan":p.pan,"area":p.area,"status":p.status,"loaded":p.pixels.is_some(),
            "actions":p.actions.iter().map(|(a,r)| serde_json::json!({"label":label(*a),"rect":r})).collect::<Vec<_>>(),
            "scale":p.scale,"dimensions":p.pixels.as_ref().map(|i|i.received),"decoded_dimensions":p.pixels.as_ref().map(|i|(i.width,i.height)),"owner":format!("{:?}",p.owner)
        }));
        let previous = if probe.step == 0 {
            "initial"
        } else {
            actions.get(probe.step - 1).copied().unwrap_or("done")
        };
        let logical_size = self.image_preview_probe_size(owner);
        let active_pane = self.ws.lock().unwrap().active_pane.clone();
        let active_surface = active_pane
            .as_deref()
            .map(|pane| self.ws.lock().unwrap().active_tab_pid(pane));
        probe.records.push(serde_json::json!({"active_pane":active_pane,"active_surface":active_surface,"window_size":logical_size,"after":previous,"hits":hits,"preview":modal,"selection":format!("{:?}",self.selection),"drag_anchor":format!("{:?}",self.drag_anchor)}));
        let report = PathBuf::from(report);
        if let Some(parent) = report.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &report,
            serde_json::to_vec_pretty(
                &serde_json::json!({"steps":probe.records,"done":probe.step >= actions.len()}),
            )
            .unwrap(),
        );
        let capture = report.with_file_name(format!("preview-{:02}-{previous}.png", probe.step));
        self.image_preview_probe_capture(owner, capture.to_string_lossy().into_owned());
        if probe.step >= actions.len() {
            probe.done = true;
            self.image_preview.probe = probe;
            return;
        }
        let action = actions[probe.step];
        let hit = self
            .image_preview
            .hits
            .get(&owner)
            .and_then(|(_, h)| h.first())
            .and_then(|h| h.rects.first())
            .copied();
        let area = self.image_preview.preview.as_ref().map(|p| p.area);
        let point = area.or(hit).map(center).unwrap_or((50.0, 50.0));
        match action {
            "click_point" => {
                if let Some(point) = explicit_point {
                    self.image_preview_probe_click(owner, point, event_loop);
                }
            }
            "copy_key" => self.image_preview_probe_native_key(owner, "c", 8, 1 << 20),
            "paste_menu" => self.paste_from_menu(),
            "click" => {
                if let Some(hit) = hit {
                    self.image_preview_probe_click(owner, center(hit), event_loop);
                }
            }
            "drag" => {
                if let Some(hit) = hit {
                    let start = center(hit);
                    self.image_preview_probe_move(owner, start, event_loop);
                    self.window_event(event_loop, owner, button(ElementState::Pressed));
                    self.image_preview_probe_move(
                        owner,
                        (start.0 + 40.0, start.1 + 20.0),
                        event_loop,
                    );
                    self.window_event(event_loop, owner, button(ElementState::Released));
                }
            }
            "zoom_in" => {
                self.image_preview_probe_move(owner, point, event_loop);
                self.window_event(
                    event_loop,
                    owner,
                    WindowEvent::MouseWheel {
                        device_id: DeviceId::dummy(),
                        delta: MouseScrollDelta::LineDelta(0.0, 8.0),
                        phase: TouchPhase::Moved,
                    },
                );
            }
            "pan_far" => {
                self.image_preview_probe_move(owner, point, event_loop);
                self.window_event(event_loop, owner, button(ElementState::Pressed));
                self.image_preview_probe_move(
                    owner,
                    (point.0 + 5000.0, point.1 - 5000.0),
                    event_loop,
                );
                self.window_event(event_loop, owner, button(ElementState::Released));
            }
            "ime" => {
                self.window_event(
                    event_loop,
                    owner,
                    WindowEvent::Ime(winit::event::Ime::Preedit("검증".into(), None)),
                );
                self.window_event(
                    event_loop,
                    owner,
                    WindowEvent::Ime(winit::event::Ime::Commit("preview-must-not-leak".into())),
                );
            }
            "escape" => self.image_preview_probe_native_key(owner, "\u{1b}", 53, 0),
            _ => {
                let action = match action {
                    "zoom_out" => Some(Action::Out),
                    "original" => Some(Action::Actual),
                    "fit" => Some(Action::Fit),
                    "copy" => Some(Action::Copy),
                    "save" => Some(Action::Save),
                    "close" => Some(Action::Close),
                    _ => None,
                };
                if let Some(rect) = action.and_then(|a| {
                    self.image_preview
                        .preview
                        .as_ref()?
                        .actions
                        .iter()
                        .find(|(b, _)| *b == a)
                        .map(|(_, r)| *r)
                }) {
                    self.image_preview_probe_click(owner, center(rect), event_loop);
                }
            }
        }
        if action == "click" && std::env::var_os("KASATERM_TEST_IMAGE_PREVIEW_IDLE").is_some() {
            probe.done = true;
            self.image_preview.probe = probe;
            return;
        }
        probe.step += 1;
        probe.next = Some(Instant::now() + std::time::Duration::from_millis(700));
        wait(self.proxy.clone(), 750);
        self.image_preview.probe = probe;
    }
    fn image_preview_probe_move(
        &mut self,
        owner: WindowId,
        point: Point,
        event_loop: &ActiveEventLoop,
    ) {
        let scale = if self.window.as_ref().is_some_and(|w| w.id() == owner) {
            self.effective_scale()
        } else {
            self.aux
                .terminals
                .iter()
                .find(|t| t.window.id() == owner)
                .map(|t| t.window.scale_factor() as f32)
                .unwrap_or(1.0)
        };
        self.window_event(
            event_loop,
            owner,
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(
                    (point.0 * scale) as f64,
                    (point.1 * scale) as f64,
                ),
            },
        );
    }
    fn image_preview_probe_click(
        &mut self,
        owner: WindowId,
        point: Point,
        event_loop: &ActiveEventLoop,
    ) {
        self.image_preview_probe_move(owner, point, event_loop);
        self.window_event(event_loop, owner, button(ElementState::Pressed));
        self.window_event(event_loop, owner, button(ElementState::Released));
    }
    fn image_preview_probe_capture(&mut self, owner: WindowId, path: String) {
        if self.window.as_ref().is_some_and(|w| w.id() == owner) {
            if let Some(g) = &mut self.gpu {
                g.capture_next = Some(path);
            }
            self.chrome_dirty = true;
            self.render_frame();
        } else {
            self.aux_preview_probe_capture(owner, path);
        }
    }
    #[cfg(target_os = "macos")]
    fn image_preview_probe_native_key(
        &self,
        owner: WindowId,
        characters: &str,
        keycode: u16,
        flags: u64,
    ) {
        use objc2::{
            msg_send,
            runtime::{AnyClass, AnyObject},
        };
        use objc2_foundation::{NSPoint, NSString};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let window = self
            .window
            .as_ref()
            .filter(|w| w.id() == owner)
            .or_else(|| {
                self.aux
                    .terminals
                    .iter()
                    .find(|t| t.window.id() == owner)
                    .map(|t| &t.window)
            });
        let Some(window) = window else { return };
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return;
        };
        unsafe {
            let view = h.ns_view.as_ptr() as *mut AnyObject;
            let ns_window: *mut AnyObject = msg_send![view, window];
            let Some(class) = AnyClass::get(c"NSEvent") else {
                return;
            };
            let number: isize = msg_send![ns_window, windowNumber];
            let chars = NSString::from_str(characters);
            for kind in [10u64, 11] {
                let event: *mut AnyObject = msg_send![class,keyEventWithType:kind,location:NSPoint::new(0.0,0.0),modifierFlags:flags,timestamp:0.0f64,windowNumber:number,context:std::ptr::null_mut::<AnyObject>(),characters:&*chars,charactersIgnoringModifiers:&*chars,isARepeat:false,keyCode:keycode];
                if kind == 10 {
                    let _: () = msg_send![view,keyDown:event];
                } else {
                    let _: () = msg_send![view,keyUp:event];
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    fn image_preview_probe_native_key(
        &mut self,
        _owner: WindowId,
        _characters: &str,
        _keycode: u16,
        _flags: u64,
    ) {
    }
}
fn button(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: MouseButton::Left,
    }
}
fn center(r: Rect) -> Point {
    (r.0 + r.2 * 0.5, r.1 + r.3 * 0.5)
}
fn label(a: Action) -> &'static str {
    match a {
        Action::In => "zoom_in",
        Action::Out => "zoom_out",
        Action::Fit => "fit",
        Action::Actual => "original",
        Action::Copy => "copy",
        Action::Save => "save",
        Action::Close => "close",
    }
}
