use super::*;
use crate::image_preview::Response;
use crate::inline_media::{self, cache, Rect, Reference};
use kasa_gridview::images::InlineSlot;
use std::sync::atomic::Ordering;
use winit::keyboard::PhysicalKey;

mod probe;
type Point = (f32, f32);
#[derive(Clone)]
struct Hit {
    reference: Reference,
    rect: Rect,
}
struct Press {
    owner: WindowId,
    hit: Hit,
    point: Point,
    event: WindowEvent,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Close,
    Toggle,
    Back,
    Forward,
    Mute,
    Retry,
}
struct Preview {
    owner: WindowId,
    context: usize,
    reference: Reference,
    generation: u64,
    asset: Option<Arc<cache::Asset>>,
    download: Option<Arc<cache::Download>>,
    player: Option<crate::media_player::Player>,
    error: Option<String>,
    actions: Vec<(Action, Rect)>,
    seek: Rect,
    dragging: bool,
    muted: bool,
    focus: usize,
    frame_serial: u64,
}
impl Drop for Preview {
    fn drop(&mut self) {
        if let Some(job) = &self.download {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }
}
#[derive(Default)]
pub(crate) struct State {
    hits: HashMap<WindowId, (usize, Vec<Hit>)>,
    press: Option<Press>,
    preview: Option<Preview>,
    pub replay: bool,
    generation: u64,
    suppress_key: Option<(WindowId, PhysicalKey)>,
    consumed_button: Option<(WindowId, MouseButton)>,
    focused: Option<WindowId>,
    probe_at: Option<Instant>,
}
fn contains(r: Rect, p: Point) -> bool {
    r.2 > 0.0 && r.3 > 0.0 && p.0 >= r.0 && p.1 >= r.1 && p.0 < r.0 + r.2 && p.1 < r.1 + r.3
}
fn moved(a: Point, b: Point) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) > 4.0
}
impl State {
    pub fn close_owner(&mut self, id: WindowId) {
        if self.owns(id) {
            self.preview = None;
        }
        self.hits.remove(&id);
        if self.press.as_ref().is_some_and(|p| p.owner == id) {
            self.press = None;
        }
        if self.focused == Some(id) {
            self.focused = None;
        }
        if self.suppress_key.is_some_and(|(owner, _)| owner == id) {
            self.suppress_key = None;
        }
        if self.consumed_button.is_some_and(|(owner, _)| owner == id) {
            self.consumed_button = None;
        }
    }
    pub fn owns(&self, id: WindowId) -> bool {
        self.preview.as_ref().is_some_and(|p| p.owner == id)
    }
    pub fn focused(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|p| self.focused == Some(p.owner))
    }
    pub fn active(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| {
            p.error.is_none()
                && p.player.as_ref().is_none_or(|player| {
                    let s = player.state();
                    s.playing || s.buffering || s.seeking
                })
        })
    }
    pub fn hit(&self, id: WindowId, point: Point) -> bool {
        self.hits
            .get(&id)
            .is_some_and(|(_, hits)| hits.iter().any(|h| contains(h.rect, point)))
    }
    pub fn collect(&mut self, id: WindowId, context: usize, slots: &[InlineSlot]) {
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.owner == id && p.context != context)
        {
            self.preview = None;
        }
        self.hits.insert(
            id,
            (
                context,
                slots
                    .iter()
                    .filter_map(|s| {
                        Reference::from_slot(s).map(|reference| Hit {
                            reference,
                            rect: inline_media::intersection(s.rect, s.clip),
                        })
                    })
                    .collect(),
            ),
        );
    }
    pub fn cursor(&self, id: WindowId, point: Point) -> CursorIcon {
        if self.hit(id, point)
            || self.preview.as_ref().is_some_and(|p| {
                p.owner == id
                    && (p.actions.iter().any(|(_, r)| contains(*r, point))
                        || contains(p.seek, point))
            })
        {
            CursorIcon::Pointer
        } else {
            CursorIcon::Default
        }
    }
    fn open(&mut self, id: WindowId, reference: Reference, proxy: &EventLoopProxy<UserEvent>) {
        cache::request(&reference, proxy, true);
        self.generation += 1;
        self.focused = Some(id);
        self.preview = Some(Preview {
            owner: id,
            context: self.hits.get(&id).map_or(0, |h| h.0),
            reference,
            generation: self.generation,
            asset: None,
            download: None,
            player: None,
            error: None,
            actions: Vec::new(),
            seek: (0.0, 0.0, 0.0, 0.0),
            dragging: false,
            muted: false,
            focus: 0,
            frame_serial: 0,
        });
    }
    pub fn tick(&mut self, proxy: &EventLoopProxy<UserEvent>) -> bool {
        let Some(p) = self.preview.as_mut() else {
            return false;
        };
        let mut changed = false;
        if p.asset.is_none() && p.error.is_none() {
            match cache::get(&p.reference) {
                Ok(Some(asset)) => {
                    p.download = Some(cache::download(
                        p.reference.clone(),
                        asset.clone(),
                        proxy.clone(),
                    ));
                    p.asset = Some(asset);
                    changed = true;
                }
                Err(error) => {
                    p.error = Some(error);
                    changed = true;
                }
                Ok(None) => {
                    cache::request(&p.reference, proxy, false);
                }
            }
        }
        let result = p
            .download
            .as_ref()
            .and_then(|job| job.result.lock().unwrap().take());
        if let Some(result) = result {
            p.download = None;
            changed = true;
            match result.and_then(|path| crate::media_player::Player::open(&path)) {
                Ok(mut player) => {
                    if crate::verification_run() {
                        player.set_volume(0.0);
                        p.muted = true;
                    }
                    if self.focused == Some(p.owner) {
                        player.play();
                    }
                    p.player = Some(player);
                }
                Err(error) => p.error = Some(error),
            }
        }
        changed
    }
    fn action(&mut self, action: Action, proxy: &EventLoopProxy<UserEvent>) {
        if action == Action::Close {
            self.preview = None;
            return;
        }
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        if action == Action::Retry {
            let owner = p.owner;
            let reference = p.reference.clone();
            self.open(owner, reference, proxy);
            return;
        }
        let Some(player) = &mut p.player else {
            return;
        };
        let s = player.state();
        match action {
            Action::Toggle => {
                if s.playing {
                    player.pause();
                } else {
                    if s.ended {
                        player.seek(0.0);
                    }
                    player.play();
                }
            }
            Action::Back => player.seek((s.position - 5.0).max(0.0)),
            Action::Forward => {
                player.seek((s.position + 5.0).min(s.duration.unwrap_or(s.position + 5.0)))
            }
            Action::Mute => {
                p.muted = !p.muted;
                player.set_volume(if p.muted { 0.0 } else { 1.0 });
            }
            _ => {}
        }
    }
    pub fn event(
        &mut self,
        owner: WindowId,
        point: Point,
        modifiers: ModifiersState,
        event: &WindowEvent,
        proxy: &EventLoopProxy<UserEvent>,
        allow: bool,
    ) -> Response {
        if self.replay {
            return Response::Pass;
        }
        if let WindowEvent::Focused(focused) = event {
            if *focused {
                self.focused = Some(owner);
            } else if self.focused == Some(owner) {
                self.focused = None;
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event {
            if self.suppress_key == Some((owner, event.physical_key)) {
                if event.state == ElementState::Released {
                    self.suppress_key = None;
                }
                return Response::Consumed;
            }
        }
        if let WindowEvent::MouseInput { state, button, .. } = event {
            if !state.is_pressed() && self.consumed_button == Some((owner, *button)) {
                self.consumed_button = None;
                if !self.owns(owner) {
                    return Response::Consumed;
                }
            } else if state.is_pressed() && self.owns(owner) {
                self.consumed_button = Some((owner, *button));
            }
        }
        if matches!(event, WindowEvent::Focused(false) | WindowEvent::Destroyed) {
            self.press = None;
            self.suppress_key = None;
            self.consumed_button = None;
        }
        if self.owns(owner) {
            let p = self.preview.as_mut().unwrap();
            let mut action = None;
            match event {
                WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                    self.preview = None;
                    self.hits.remove(&owner);
                    return Response::Pass;
                }
                WindowEvent::Focused(false) => {
                    if let Some(player) = &mut p.player {
                        player.pause();
                    }
                    p.dragging = false;
                    return Response::Pass;
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state.is_pressed() {
                        action = match &event.logical_key {
                            Key::Named(NamedKey::Escape) => Some(Action::Close),
                            Key::Named(NamedKey::Space) => Some(Action::Toggle),
                            Key::Named(NamedKey::ArrowLeft) => Some(Action::Back),
                            Key::Named(NamedKey::ArrowRight) => Some(Action::Forward),
                            Key::Named(NamedKey::Enter) => p.actions.get(p.focus).map(|(a, _)| *a),
                            Key::Named(NamedKey::Tab) => {
                                if !p.actions.is_empty() {
                                    p.focus = (p.focus
                                        + if modifiers.shift_key() {
                                            p.actions.len() - 1
                                        } else {
                                            1
                                        })
                                        % p.actions.len();
                                }
                                None
                            }
                            Key::Character(k) if k == "m" => Some(Action::Mute),
                            _ => None,
                        };
                        if action == Some(Action::Close) {
                            self.suppress_key = Some((owner, event.physical_key));
                        }
                    }
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    if state.is_pressed() {
                        action = p
                            .actions
                            .iter()
                            .find(|(_, r)| contains(*r, point))
                            .map(|(a, _)| *a);
                        p.dragging = contains(p.seek, point)
                            && p.player.as_ref().and_then(|v| v.state().duration).is_some();
                        if p.dragging {
                            p.seek_to(point);
                        }
                    } else {
                        if p.dragging {
                            p.seek_to(point);
                        }
                        p.dragging = false;
                    }
                }
                WindowEvent::CursorMoved { .. } => {
                    if p.dragging {
                        p.seek_to(point);
                    }
                }
                WindowEvent::Ime(_)
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::PinchGesture { .. }
                | WindowEvent::DroppedFile(_) => {}
                _ => return Response::Pass,
            }
            if let Some(action) = action {
                self.action(action, proxy);
            }
            return Response::Consumed;
        }
        if let Some(press) = self.press.as_ref().filter(|p| p.owner == owner) {
            match event {
                WindowEvent::CursorMoved { .. } if moved(press.point, point) => {
                    let p = self.press.take().unwrap();
                    return Response::Replay(p.event, p.point);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => {
                    let p = self.press.take().unwrap();
                    if allow
                        && !moved(p.point, point)
                        && self.hits.get(&owner).is_some_and(|(_, hits)| {
                            hits.iter()
                                .any(|h| h.reference == p.hit.reference && contains(h.rect, point))
                        })
                    {
                        self.open(owner, p.hit.reference, proxy);
                    }
                    return Response::Consumed;
                }
                WindowEvent::Focused(false)
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. } => {
                    self.press = None;
                }
                WindowEvent::CursorMoved { .. } => return Response::Consumed,
                _ => {}
            }
        }
        if allow
            && !modifiers.alt_key()
            && !modifiers.super_key()
            && !modifiers.control_key()
            && matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                }
            )
        {
            if let Some(hit) = self
                .hits
                .get(&owner)
                .and_then(|(_, hits)| hits.iter().rev().find(|h| contains(h.rect, point)))
                .cloned()
            {
                self.press = Some(Press {
                    owner,
                    hit,
                    point,
                    event: event.clone(),
                });
                return Response::Consumed;
            }
        }
        Response::Pass
    }
    pub fn paint(&mut self, owner: WindowId, g: &mut gpu::GpuRenderer, size: Point, cursor: Point) {
        let prefix = "media-preview:";
        let Some(p) = self.preview.as_mut().filter(|p| p.owner == owner) else {
            g.drop_images_with_prefix(prefix);
            self.observe(owner, g);
            return;
        };
        let (w, h) = size;
        let audio = p.asset.as_ref().is_some_and(|a| a.media.kind == "audio");
        let ow = (w - 32.0)
            .max(0.0)
            .min(if audio { 640.0 } else { f32::MAX });
        let oh = (h - 32.0)
            .max(0.0)
            .min(if audio { 240.0 } else { f32::MAX });
        let outer = ((w - ow) * 0.5, (h - oh) * 0.5, ow, oh);
        g.rect(0.0, 0.0, w, h, theme::with_alpha(theme::bg(), 235));
        crate::native_controls::panel(g, outer, theme::surface());
        let x = outer.0 + 16.0;
        let right = (outer.0 + outer.2 - 16.0).max(x);
        let mut y = outer.1 + 16.0;
        p.actions.clear();
        let close = crate::native_controls::icon_button(
            g,
            ((right - 32.0).max(x), y, 32.0, 32.0),
            cursor,
            "x",
            Default::default(),
        );
        p.actions.push((Action::Close, close));
        inline_media::text(
            g,
            (x, y + 8.0, (right - x - 40.0).max(0.0), 24.0),
            p.asset.as_ref().map_or("미디어", |a| a.media.name.as_str()),
            false,
        );
        y += 40.0;
        let mut bx = x;
        let s = p.player.as_ref().map(|player| player.state());
        for (action, label) in [
            (
                Action::Toggle,
                if s.as_ref().is_some_and(|s| s.playing) {
                    "일시정지"
                } else {
                    "재생"
                },
            ),
            (Action::Back, "−5초"),
            (Action::Forward, "+5초"),
            (
                Action::Mute,
                if p.muted {
                    "소리 켜기"
                } else {
                    "음소거"
                },
            ),
            (Action::Retry, "다시 받기"),
        ] {
            if action == Action::Retry
                && p.error.is_none()
                && s.as_ref().and_then(|s| s.error.as_ref()).is_none()
            {
                continue;
            }
            let width = if action == Action::Toggle {
                32.0
            } else {
                crate::native_controls::button_width(g, label, 32.0, false)
            }
            .min((right - x).max(0.0));
            if bx > x && bx + width > right {
                bx = x;
                y += 40.0;
            }
            let enabled = p.player.is_some() || action == Action::Retry;
            let style = crate::native_controls::Style {
                enabled,
                ..Default::default()
            };
            let r = if action == Action::Toggle {
                crate::native_controls::icon_button(
                    g,
                    (bx, y, width, 32.0),
                    cursor,
                    if s.as_ref().is_some_and(|s| s.playing) {
                        "pause"
                    } else {
                        "play"
                    },
                    style,
                )
            } else {
                crate::native_controls::text_button(g, (bx, y, width, 32.0), cursor, label, style)
            };
            if enabled {
                p.actions.push((action, r));
            }
            bx += width + 8.0;
        }
        y += 40.0;
        let status = if let Some(error) = p
            .error
            .as_ref()
            .or_else(|| s.as_ref().and_then(|s| s.error.as_ref()))
        {
            error.clone()
        } else if let Some(s) = &s {
            format!(
                "{} / {}{}",
                inline_media::time(s.position),
                s.duration
                    .map(inline_media::time)
                    .unwrap_or_else(|| "—".into()),
                if s.buffering {
                    " · 준비 중…"
                } else if s.ended {
                    " · 재생 완료"
                } else {
                    ""
                }
            )
        } else if let Some(job) = &p.download {
            format!(
                "미디어 받는 중 · {} / {} MiB",
                job.done.load(Ordering::Relaxed) / 1024 / 1024,
                job.total.load(Ordering::Relaxed).div_ceil(1024 * 1024)
            )
        } else {
            "미디어를 불러오는 중…".into()
        };
        inline_media::text(g, (x, y, right - x, 24.0), &status, true);
        y += 24.0;
        p.seek = (x, y, (right - x).max(0.0), 24.0);
        g.rect(x, y + 10.0, p.seek.2, 4.0, theme::border());
        if let Some(s) = &s {
            if let Some(d) = s.duration.filter(|d| *d > 0.0) {
                g.rect(
                    x,
                    y + 10.0,
                    p.seek.2 * (s.position / d).clamp(0.0, 1.0) as f32,
                    4.0,
                    theme::accent(),
                );
            }
        }
        y += 32.0;
        let area = (
            x,
            y,
            (right - x).max(0.0),
            (outer.1 + outer.3 - 16.0 - y).max(0.0),
        );
        if let Some((_, r)) = p
            .actions
            .get(p.focus.min(p.actions.len().saturating_sub(1)))
        {
            crate::native_controls::focus_ring(g, *r);
        }
        let key = format!("{prefix}{}", p.generation);
        if let Some(player) = &mut p.player {
            if let Some(frame) = player.take_frame() {
                p.frame_serial = frame.serial;
                g.update_image(&key, &frame.rgba, frame.width, frame.height);
            }
        }
        if !g.has_image(&key) {
            if let Some(poster) = p.asset.as_ref().and_then(|a| a.poster.as_ref()) {
                g.upload_image(&key, &poster.rgba, poster.width, poster.height);
            }
        }
        g.images.retain(|k, _| !k.starts_with(prefix) || k == &key);
        if g.has_image(&key) {
            g.queue_image_contain(&key, area.0, area.1, area.2, area.3);
        } else if p.asset.as_ref().is_some_and(|a| a.media.kind == "audio") {
            inline_media::text(
                g,
                (x, y + 16.0, area.2, 24.0),
                "Space 재생·일시정지  ·  ← → 5초 이동  ·  Esc 닫기",
                true,
            );
        }
        self.observe(owner, g);
    }
}
impl Preview {
    fn seek_to(&mut self, point: Point) {
        if self.seek.2 <= 0.0 {
            return;
        }
        if let Some(player) = &mut self.player {
            if let Some(d) = player.state().duration {
                player.seek(d * ((point.0 - self.seek.0) / self.seek.2).clamp(0.0, 1.0) as f64);
            }
        }
    }
}

impl App {
    pub(crate) fn focus_preview_surface(&mut self, point: Point) {
        if let Some((pane, _, _)) = self.px_to_pane_cell(point.0, point.1) {
            let (surface, switched) = {
                let ws = self.ws.lock().unwrap();
                (
                    ws.active_tab_pid(&pane),
                    ws.active_pane.as_deref() != Some(pane.as_str()),
                )
            };
            self.focus_surface(&surface);
            if switched {
                self.selection = None;
                self.drag_anchor = None;
                self.mouse_forward_pane = None;
            }
        }
    }
}
