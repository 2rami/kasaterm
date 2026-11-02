use super::*;
impl State {
    pub(super) fn observe(&mut self, owner: WindowId, g: &gpu::GpuRenderer) {
        if !crate::verification_run() {
            return;
        }
        let Some(path) = std::env::var_os("KASATERM_TEST_MEDIA_REPORT") else {
            return;
        };
        if self
            .probe_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(100))
        {
            return;
        }
        self.probe_at = Some(Instant::now());
        let hits=self.hits.get(&owner).map(|(_,hits)|hits.iter().map(|h|{
            let asset=cache::get(&h.reference);
            serde_json::json!({"id":h.reference.id,"rect":h.rect,"ready":asset.as_ref().is_ok_and(|v|v.is_some()),"error":asset.err(),
                "controls":h.controls.iter().map(|(c,r)|serde_json::json!({"control":format!("{c:?}"),"rect":r})).collect::<Vec<_>>()})
        }).collect::<Vec<_>>()).unwrap_or_default();
        let session=|s:&super::Session|{
            let state=s.state().map(|s|serde_json::json!({"position":s.position,"duration":s.duration,"playing":s.playing,"buffering":s.buffering,"seeking":s.seeking,"ended":s.ended,"error":s.error}));
            serde_json::json!({"id":s.reference.id,"state":state,"error":s.error,"received":s.stream.as_ref().map(Stream::received_bytes),"stream_error":s.stream.as_ref().and_then(Stream::error),
                "size":s.asset.as_ref().map(|a|a.media.size),"muted":s.volume.muted,"volume":s.volume.value(),"frames":s.frame_serial,"drawn":g.drawn_image_clipped_rects(&s.key())})
        };
        let preview=self.preview.as_ref().filter(|p|p.owner==owner).map(|p|{
            let mut v=session(&p.session);
            v["seek"]=serde_json::json!(p.seek);
            v["volume_track"]=serde_json::json!(p.volume_track);
            v["from_card"]=serde_json::json!(p.from_card);
            v["actions"]=serde_json::json!(p.actions.iter().map(|(a,r)|serde_json::json!({"action":format!("{a:?}"),"rect":r})).collect::<Vec<_>>());
            v
        });
        let inline=self.inline.as_ref().filter(|i|i.owner==owner).map(|i|session(&i.session));
        let saving=self.saving.lock().map(|s|s.len()).unwrap_or(0);
        let _ = std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({"hits":hits,"preview":preview,"inline":inline,"saving":saving,
                "volume":self.volume.value(),"muted":self.volume.muted,"volume_dragging":self.volume_drag.is_some()})).unwrap(),
        );
    }
}

impl App {
    pub(crate) fn run_pending_media_volume_probe(&mut self, event_loop: &ActiveEventLoop) {
        if !crate::verification_run() || std::env::var_os("KASATERM_INLINE_MEDIA_DIR").is_none()
            || std::env::var_os("KASATERM_SETTINGS_FILE").is_none() { return; }
        let Ok(folder) = std::env::var("KASATERM_AUTOVOLUME_DIR") else { return };
        let (Ok(audio), Ok(video)) = (std::env::var("KASATERM_VOLUME_AUDIO"), std::env::var("KASATERM_VOLUME_VIDEO")) else { return };
        static STEP: std::sync::OnceLock<std::sync::Mutex<(Instant, usize, u64)>> = std::sync::OnceLock::new();
        let mut step = STEP.get_or_init(|| std::sync::Mutex::new((Instant::now(), 0, 0))).lock().unwrap();
        if step.1 > 8 || step.0.elapsed().as_millis() < 800 { return; }
        let Some(window) = self.window.as_ref().map(|w| w.id()) else { return };
        let audio = Reference { id: audio, source: None };
        let video = Reference { id: video, source: None };
        let control = |app: &Self, reference: &Reference, which: Control| {
            app.media_preview.hits.get(&window).and_then(|(_, hits)| hits.iter().find(|h| h.reference == *reference))
                .and_then(|h| h.controls.iter().find(|(c, _)| *c == which).map(|(_, r)| *r))
        };
        if step.1 == 0 && (control(self, &audio, Control::Mute).is_none() || control(self, &video, Control::Mute).is_none()) { return; }
        if step.1 == 3 && self.media_preview.inline.as_ref().and_then(|i| i.session.state())
            .is_none_or(|s| s.buffering || s.seeking || s.duration.is_none()) { return; }
        if step.1 == 7 && self.media_preview.inline.as_ref().is_none_or(|i| i.session.frame_serial == 0) { return; }
        let move_to = |app: &mut Self, point: Point| {
            let scale = app.effective_scale();
            app.window_event(event_loop, window, WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new((point.0 * scale) as f64, (point.1 * scale) as f64),
            });
        };
        let button = |app: &mut Self, state| app.window_event(event_loop, window, WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(), state, button: MouseButton::Left,
        });
        let click = |app: &mut Self, rect: Rect| {
            move_to(app, (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0));
            button(app, ElementState::Pressed);
            button(app, ElementState::Released);
            app.chrome_dirty = true;
            app.render_frame();
        };
        let adjust = |app: &mut Self, rect: Rect, fraction: f32| {
            move_to(app, (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0));
            app.chrome_dirty = true;
            app.render_frame();
            button(app, ElementState::Pressed);
            move_to(app, (rect.0 + rect.2 * fraction, rect.1 + rect.3 / 2.0));
            app.chrome_dirty = true;
            app.render_frame();
            button(app, ElementState::Released);
            app.chrome_dirty = true;
            app.render_frame();
        };
        let reveal = |app: &mut Self, reference: &Reference| {
            if let Some(rect) = control(app, reference, Control::Mute) {
                move_to(app, (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0));
                app.chrome_dirty = true;
                app.render_frame();
            }
            control(app, reference, Control::Volume)
        };
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        let mut checks = serde_json::Map::new();
        let mut check = |name: &str, value: bool| { checks.insert(name.into(), serde_json::json!(value)); };
        let stage = step.1;
        match stage {
            0 => {
                move_to(self, (900.0, 700.0));
                self.chrome_dirty = true;
                self.render_frame();
                check("sliders_hidden_without_hover", control(self, &audio, Control::Volume).is_none() && control(self, &video, Control::Volume).is_none());
                check("audio_volume_before_play", reveal(self, &audio).is_some());
                check("video_volume_before_play", reveal(self, &video).is_some());
                check("only_hovered_card_expanded", control(self, &audio, Control::Volume).is_none());
                check("no_autoplay", !self.media_preview.active() && self.media_preview.inline.is_none());
                if std::env::var_os("KASATERM_VOLUME_RESTORE").is_some() {
                    check("restored_volume", near(self.media_preview.volume.value(), 0.75));
                    step.1 = 8;
                }
            }
            1 => {
                if let Some(rect) = reveal(self, &audio) { adjust(self, rect, 0.25); }
                check("preplay_level", near(self.media_preview.volume.value(), 0.25));
                check("slider_stays_open_over_track", control(self, &audio, Control::Volume).is_some());
                check("volume_does_not_start_player", self.media_preview.inline.is_none() && self.media_preview.preview.is_none());
            }
            2 => {
                if let Some(rect) = control(self, &audio, Control::Toggle) { click(self, rect); }
                check("audio_started", self.media_preview.inline.as_ref().is_some_and(|i| i.session.reference == audio));
                check("audio_keeps_level", self.media_preview.inline.as_ref().is_some_and(|i| near(i.session.volume.value(), 0.25)));
                step.2 = self.media_preview.generation;
            }
            3 => {
                if self.media_preview.inline.as_ref().and_then(|i| i.session.state()).is_some_and(|s| s.playing) {
                    if let Some(rect) = control(self, &audio, Control::Toggle) { click(self, rect); }
                }
                let before = self.media_preview.inline.as_ref().and_then(|i| i.session.state()).map(|s| s.position);
                let rect = reveal(self, &audio);
                check("audio_volume_control", rect.is_some());
                if let Some(rect) = rect { adjust(self, rect, 0.75); }
                let after = self.media_preview.inline.as_ref().and_then(|i| i.session.state());
                check("audio_level", near(self.media_preview.volume.value(), 0.75));
                check("volume_does_not_seek", before.zip(after.as_ref()).is_some_and(|(before, after)| (before - after.position).abs() < 0.15));
                check("volume_does_not_resume", after.is_some_and(|s| !s.playing));
                check("player_not_recreated", self.media_preview.generation == step.2);
            }
            4 => {
                let mute = control(self, &audio, Control::Mute);
                check("mute_control", mute.is_some());
                if let Some(rect) = mute {
                    click(self, rect);
                    check("muted", self.media_preview.volume.value() == 0.0);
                    click(self, rect);
                    check("unmute_restores_level", near(self.media_preview.volume.value(), 0.75));
                }
                if let Some(rect) = control(self, &audio, Control::Expand) { click(self, rect); }
                check("expanded_keeps_level", self.media_preview.preview.as_ref().is_some_and(|p| near(p.session.volume.value(), 0.75)));
            }
            5 => {
                let track = self.media_preview.preview.as_ref().map(|p| p.volume_track);
                check("preview_volume_control", track.is_some_and(|r| r.2 > 0.0));
                if let Some(rect) = track { adjust(self, rect, 0.5); }
                check("preview_level", self.media_preview.preview.as_ref().is_some_and(|p| near(p.session.volume.value(), 0.5)));
            }
            6 => {
                let close = self.media_preview.preview.as_ref().and_then(|p| p.actions.iter().find(|(a, _)| *a == Action::Close).map(|(_, r)| *r));
                check("preview_close_control", close.is_some());
                if let Some(rect) = close { click(self, rect); }
                check("return_to_card_keeps_level", self.media_preview.inline.as_ref().is_some_and(|i| near(i.session.volume.value(), 0.5)));
                let play = control(self, &video, Control::Toggle);
                check("video_play_control", play.is_some());
                if let Some(rect) = play { click(self, rect); }
                check("next_card_keeps_level", self.media_preview.inline.as_ref().is_some_and(|i| i.session.reference == video && near(i.session.volume.value(), 0.5)));
            }
            7 => {
                check("video_decoded", self.media_preview.inline.as_ref().is_some_and(|i| i.session.frame_serial > 0 && i.session.error.is_none()));
                let track = reveal(self, &video);
                check("video_volume_control", track.is_some());
                if let Some(rect) = track { adjust(self, rect, 0.75); }
                check("video_level", self.media_preview.inline.as_ref().is_some_and(|i| near(i.session.volume.value(), 0.75)));
                check("drag_released", self.media_preview.volume_drag.is_none());
            }
            8 => {
                move_to(self, (900.0, 700.0));
                self.chrome_dirty = true;
                self.render_frame();
                check("slider_closes_after_leaving", control(self, &video, Control::Volume).is_none());
                let volume = Volume::load(&socket::read_settings());
                check("saved_volume", near(volume.value(), 0.75));
            }
            _ => {}
        }
        let root = std::path::Path::new(&folder);
        let _ = std::fs::create_dir_all(root);
        let _ = std::fs::write(root.join(format!("{stage:02}.json")), serde_json::json!({"checks":checks}).to_string());
        self.chrome_dirty = true;
        if let Some(g) = &mut self.gpu { g.capture_next = Some(root.join(format!("{stage:02}.png")).to_string_lossy().into_owned()); }
        self.render_frame();
        step.0 = Instant::now();
        step.1 += 1;
    }
}
