use super::*;
use kasa_gridview::images::InlineSlot;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
mod probe;
use winit::keyboard::PhysicalKey;

type Rect = (f32, f32, f32, f32);
type Point = (f32, f32);

#[derive(Clone)]
struct Hit {
    key: String,
    path: String,
    rects: Vec<Rect>,
}
struct Press {
    owner: WindowId,
    hit: Hit,
    point: Point,
    event: WindowEvent,
}
struct Pixels {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    received: (u32, u32),
    encoded: Vec<u8>,
    extension: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Out,
    In,
    Fit,
    Actual,
    Copy,
    Save,
    Close,
}
struct Preview {
    owner: WindowId,
    context: usize,
    generation: u64,
    pixels: Option<Arc<Pixels>>,
    result: Arc<Mutex<Option<Result<Arc<Pixels>, String>>>>,
    operation: Arc<Mutex<Option<Result<String, String>>>>,
    busy: bool,
    status: String,
    zoom: f32,
    pan: Point,
    drag: Option<Point>,
    area: Rect,
    scale: f32,
    actions: Vec<(Action, Rect)>,
    focus: usize,
}
#[derive(Default)]
pub(crate) struct State {
    hits: HashMap<WindowId, (usize, Vec<Hit>)>,
    press: Option<Press>,
    preview: Option<Preview>,
    next_generation: u64,
    pub(crate) replay: bool,
    suppress_key: Option<(WindowId, PhysicalKey)>,
    focused: Option<WindowId>,
    consumed_button: Option<(WindowId, MouseButton)>,
    probe: probe::Probe,
}
pub(crate) enum Response {
    Pass,
    Consumed,
    Replay(WindowEvent, Point),
}

fn contains(r: Rect, p: Point) -> bool {
    r.2 > 0.0 && r.3 > 0.0 && p.0 >= r.0 && p.1 >= r.1 && p.0 < r.0 + r.2 && p.1 < r.1 + r.3
}
fn moved(a: Point, b: Point) -> bool {
    (a.0 - b.0).hypot(a.1 - b.1) > 4.0
}
fn fit(area: Rect, dims: (u32, u32), scale: f32) -> f32 {
    ((area.2 * scale / dims.0.max(1) as f32).min(area.3 * scale / dims.1.max(1) as f32))
        .min(1.0)
        .max(f32::EPSILON)
}
fn clamp_pan(pan: Point, area: Rect, dims: (u32, u32), scale: f32, zoom: f32) -> Point {
    let factor = fit(area, dims, scale) * zoom / scale;
    let mx = ((dims.0 as f32 * factor - area.2) * 0.5).max(0.0);
    let my = ((dims.1 as f32 * factor - area.3) * 0.5).max(0.0);
    (pan.0.clamp(-mx, mx), pan.1.clamp(-my, my))
}
fn zoom_anchor(pan: Point, ratio: f32, area: Rect, anchor: Point) -> Point {
    let offset = (
        anchor.0 - area.0 - area.2 * 0.5,
        anchor.1 - area.1 - area.3 * 0.5,
    );
    (
        offset.0 * (1.0 - ratio) + pan.0 * ratio,
        offset.1 * (1.0 - ratio) + pan.1 * ratio,
    )
}

impl State {
    pub(crate) fn pending_result(&self) -> bool {
        self.preview.as_ref().is_some_and(|p| {
            p.result.lock().unwrap().is_some() || p.operation.lock().unwrap().is_some()
        })
    }
    pub(crate) fn cursor(&self, owner: WindowId, point: Point) -> CursorIcon {
        if let Some(p) = self.preview.as_ref().filter(|p| p.owner == owner) {
            if p.actions.iter().any(|(_, r)| contains(*r, point)) {
                CursorIcon::Pointer
            } else if p.drag.is_some() {
                CursorIcon::Grabbing
            } else if contains(p.area, point) {
                CursorIcon::Grab
            } else {
                CursorIcon::Default
            }
        } else if self.hit(owner, point) {
            CursorIcon::ZoomIn
        } else {
            CursorIcon::Default
        }
    }
    pub(crate) fn focused(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|p| Some(p.owner) == self.focused)
    }
    pub(crate) fn owns(&self, owner: WindowId) -> bool {
        self.preview.as_ref().is_some_and(|p| p.owner == owner)
    }
    pub(crate) fn hit(&self, owner: WindowId, point: Point) -> bool {
        self.hits.get(&owner).is_some_and(|(_, hits)| {
            hits.iter()
                .rev()
                .any(|h| h.rects.iter().any(|r| contains(*r, point)))
        })
    }
    pub(crate) fn collect(
        &mut self,
        owner: WindowId,
        context: usize,
        g: &gpu::GpuRenderer,
        slots: &[InlineSlot],
    ) {
        if self
            .preview
            .as_ref()
            .is_some_and(|p| p.owner == owner && p.context != context)
        {
            self.preview = None;
        }
        let mut seen = std::collections::HashSet::new();
        let hits = slots
            .iter()
            .filter(|slot| {
                slot.previewable
                    && (slot.key.starts_with("result:") || slot.rect.2 > 64.0 || slot.rect.3 > 64.0)
                    && seen.insert(slot.key.clone())
            })
            .filter_map(|slot| {
                let rects = g.drawn_image_clipped_rects(&slot.key);
                (!rects.is_empty()).then(|| Hit {
                    key: slot.key.clone(),
                    path: slot.path.clone(),
                    rects,
                })
            })
            .collect();
        self.hits.insert(owner, (context, hits));
    }
    /// 본문 밖에서 고른 결과 그림(Info 「결과물」)을 같은 미리보기로 연다.
    pub(crate) fn open_path(&mut self, owner: WindowId, path: String, proxy: &EventLoopProxy<UserEvent>) {
        self.open(owner, path, proxy);
    }
    fn open(&mut self, owner: WindowId, path: String, proxy: &EventLoopProxy<UserEvent>) {
        self.next_generation += 1;
        let result = Arc::new(Mutex::new(None));
        let worker_result = result.clone();
        let worker_path = path.clone();
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            if crate::verification_run() {
                if let Some(delay) = std::env::var("KASATERM_TEST_IMAGE_PREVIEW_LOAD_MS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    std::thread::sleep(std::time::Duration::from_millis(delay.min(10000)));
                }
            }
            let loaded = load(&worker_path)
                .map(Arc::new)
                .map_err(|e| format!("그림을 열지 못했어요: {e}"));
            *worker_result.lock().unwrap() = Some(loaded);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
        self.focused = Some(owner);
        self.preview = Some(Preview {
            owner,
            context: self.hits.get(&owner).map_or(0, |h| h.0),
            generation: self.next_generation,
            pixels: None,
            result,
            operation: Arc::new(Mutex::new(None)),
            busy: false,
            status: "그림을 불러오는 중…".into(),
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag: None,
            area: (0.0, 0.0, 0.0, 0.0),
            scale: 1.0,
            actions: Vec::new(),
            focus: 0,
        });
    }
    pub(crate) fn event(
        &mut self,
        owner: WindowId,
        point: Point,
        modifiers: ModifiersState,
        event: &WindowEvent,
        proxy: &EventLoopProxy<UserEvent>,
        allow_open: bool,
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
        if let WindowEvent::MouseInput { state, button, .. } = event {
            if *state == ElementState::Released && self.consumed_button == Some((owner, *button)) {
                self.consumed_button = None;
                if !self.owns(owner) {
                    return Response::Consumed;
                }
            } else if state.is_pressed() && self.owns(owner) {
                self.consumed_button = Some((owner, *button));
            }
        }
        if matches!(event, WindowEvent::Destroyed | WindowEvent::Focused(false)) {
            self.press = None;
            self.consumed_button = None;
            self.suppress_key = None;
            if matches!(event, WindowEvent::Destroyed) {
                self.hits.remove(&owner);
            }
        }
        if let WindowEvent::KeyboardInput { event, .. } = event {
            if self.suppress_key.as_ref() == Some(&(owner, event.physical_key)) {
                if event.state == ElementState::Released {
                    self.suppress_key = None;
                }
                return Response::Consumed;
            }
        }
        if self.owns(owner) {
            let p = self.preview.as_mut().unwrap();
            let mut action = None;
            match event {
                WindowEvent::CloseRequested | WindowEvent::Destroyed => {
                    self.preview = None;
                    return Response::Pass;
                }
                WindowEvent::CursorMoved { .. } => {
                    if let Some(last) = p.drag.replace(point) {
                        p.pan.0 += point.0 - last.0;
                        p.pan.1 += point.1 - last.1;
                        p.clamp();
                    } else {
                        p.drag = None;
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
                        if action.is_none() && contains(p.area, point) {
                            p.drag = Some(point);
                        }
                    } else {
                        p.drag = None;
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => *y * 0.18,
                        MouseScrollDelta::PixelDelta(d) => d.y as f32 * 0.006,
                    };
                    p.zoom_by(amount.clamp(-1.0, 1.0).exp(), point);
                }
                WindowEvent::PinchGesture { delta, .. } => {
                    p.zoom_by((1.0 + *delta as f32).max(0.1), point)
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if event.state.is_pressed() {
                        match &event.logical_key {
                            Key::Named(NamedKey::Escape) => action = Some(Action::Close),
                            Key::Named(NamedKey::Tab) => {
                                if !p.actions.is_empty() {
                                    p.focus = (p.focus
                                        + p.actions.len()
                                        + if modifiers.shift_key() {
                                            p.actions.len() - 1
                                        } else {
                                            1
                                        })
                                        % p.actions.len();
                                }
                            }
                            Key::Named(NamedKey::Enter | NamedKey::Space) if !event.repeat => {
                                action = p.actions.get(p.focus).map(|(a, _)| *a)
                            }
                            Key::Character(c) => {
                                action = match c.as_str() {
                                    "+" | "=" => Some(Action::In),
                                    "-" => Some(Action::Out),
                                    "0" => Some(Action::Fit),
                                    "1" => Some(Action::Actual),
                                    "c" if modifiers.super_key() || modifiers.control_key() => {
                                        Some(Action::Copy)
                                    }
                                    "s" if modifiers.super_key() || modifiers.control_key() => {
                                        Some(Action::Save)
                                    }
                                    _ => None,
                                }
                            }
                            _ => {}
                        }
                        if action == Some(Action::Close) {
                            self.suppress_key = Some((owner, event.physical_key));
                        }
                    }
                }
                WindowEvent::Ime(_)
                | WindowEvent::DroppedFile(_)
                | WindowEvent::Touch(_)
                | WindowEvent::MouseInput { .. } => {}
                WindowEvent::Focused(false) => {
                    p.drag = None;
                    return Response::Pass;
                }
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
                    let press = self.press.take().unwrap();
                    return Response::Replay(press.event, press.point);
                }
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => {
                    let press = self.press.take().unwrap();
                    let still_visible = !moved(press.point, point)
                        && self.hits.get(&owner).is_some_and(|(_, hits)| {
                            hits.iter().any(|h| {
                                h.key == press.hit.key
                                    && h.path == press.hit.path
                                    && h.rects.iter().any(|r| contains(*r, point))
                            })
                        });
                    if still_visible && allow_open {
                        self.open(owner, press.hit.path, proxy);
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
        if allow_open
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
                .and_then(|(_, hits)| {
                    hits.iter()
                        .rev()
                        .find(|h| h.rects.iter().any(|r| contains(*r, point)))
                })
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
    pub(crate) fn copy(&mut self, proxy: &EventLoopProxy<UserEvent>) {
        self.action(Action::Copy, proxy);
    }
    fn action(&mut self, action: Action, proxy: &EventLoopProxy<UserEvent>) {
        if action == Action::Close {
            self.preview = None;
            return;
        }
        let Some(p) = self.preview.as_mut() else {
            return;
        };
        let center = (p.area.0 + p.area.2 * 0.5, p.area.1 + p.area.3 * 0.5);
        match action {
            Action::In => p.zoom_by(1.25, center),
            Action::Out => p.zoom_by(0.8, center),
            Action::Fit => {
                p.zoom = 1.0;
                p.pan = (0.0, 0.0);
            }
            Action::Actual => {
                if let Some(image) = &p.pixels {
                    p.zoom = 1.0 / fit(p.area, (image.width, image.height), p.scale);
                    p.pan = (0.0, 0.0);
                }
            }
            Action::Copy | Action::Save if !p.busy => {
                let Some(image) = p.pixels.clone() else {
                    return;
                };
                p.busy = true;
                p.status = if action == Action::Copy {
                    "복사 중…"
                } else {
                    "저장할 위치를 고르는 중…"
                }
                .into();
                let output = p.operation.clone();
                let proxy = proxy.clone();
                std::thread::spawn(move || {
                    let result = if action == Action::Copy {
                        copy(&image)
                    } else {
                        save(&image)
                    };
                    *output.lock().unwrap() = Some(result);
                    let _ = proxy.send_event(UserEvent::Redraw);
                });
            }
            _ => {}
        }
    }
    pub(crate) fn paint(
        &mut self,
        owner: WindowId,
        g: &mut gpu::GpuRenderer,
        size: Point,
        cursor: Point,
    ) {
        let key_prefix = "image-preview:";
        let Some(p) = self.preview.as_mut().filter(|p| p.owner == owner) else {
            g.drop_images_with_prefix(key_prefix);
            return;
        };
        if let Some(result) = p.result.lock().unwrap().take() {
            match result {
                Ok(image) => {
                    p.status.clear();
                    p.pixels = Some(image);
                }
                Err(error) => p.status = error,
            }
        }
        if let Some(result) = p.operation.lock().unwrap().take() {
            p.status = result.unwrap_or_else(|e| e);
            p.busy = false;
        }
        p.scale = g.scale();
        let (w, h) = size;
        let outer = (16.0, 16.0, (w - 32.0).max(0.0), (h - 32.0).max(0.0));
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
        g.draw_text_clipped(
            x,
            y + 8.0,
            "이미지 미리보기",
            gpu::DrawOpts {
                font_size: 15.0,
                color: theme::text(),
                bold: true,
                italic: false,
            },
            x,
            (right - 40.0).max(x),
        );
        y += 40.0;
        let mut bx = x;
        for (action, label) in [
            (Action::Out, "−"),
            (Action::In, "+"),
            (Action::Fit, "맞춤"),
            (Action::Actual, "100%"),
            (Action::Copy, "복사"),
            (Action::Save, "저장"),
        ] {
            let width = crate::native_controls::button_width(g, label, 32.0, false)
                .min((right - x).max(0.0));
            if bx > x && bx + width > right {
                bx = x;
                y += 40.0;
            }
            let enabled = action == Action::Close
                || p.pixels.is_some()
                    && (!p.busy || !matches!(action, Action::Copy | Action::Save));
            let style = crate::native_controls::Style {
                enabled,
                ..Default::default()
            };
            let rect = if matches!(action, Action::Out | Action::In) {
                crate::native_controls::icon_button(
                    g,
                    (bx, y, width, 32.0),
                    cursor,
                    if action == Action::Out {
                        "minus"
                    } else {
                        "plus"
                    },
                    style,
                )
            } else {
                crate::native_controls::text_button(g, (bx, y, width, 32.0), cursor, label, style)
            };
            if enabled {
                p.actions.push((action, rect));
            }
            bx += width + 8.0;
        }
        y += 40.0;
        let status = if let Some(i) = &p.pixels {
            let dimensions = if i.received == (i.width, i.height) {
                format!("{} × {} px", i.width, i.height)
            } else {
                format!(
                    "축소 미리보기 {} × {} px · 원본 {} × {} px",
                    i.width, i.height, i.received.0, i.received.1
                )
            };
            let operation = if p.status.is_empty() {
                String::new()
            } else {
                format!("{} · ", p.status)
            };
            format!(
                "{operation}{dimensions} · {:.0}%",
                fit(p.area, (i.width, i.height), p.scale) * p.zoom * 100.0
            )
        } else {
            p.status.clone()
        };
        g.draw_text_clipped(
            x,
            y,
            &status,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
            x,
            right,
        );
        y += 24.0;
        p.area = (
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
        if let Some(image) = p.pixels.clone() {
            let key = format!("{key_prefix}{}", p.generation);
            if !g.has_image(&key) {
                g.drop_images_with_prefix(key_prefix);
                g.upload_image(&key, &image.rgba, image.width, image.height);
            }
            p.clamp();
            g.push_clip(p.area.0, p.area.1, p.area.2, p.area.3);
            g.queue_image(
                &key, p.area.0, p.area.1, p.area.2, p.area.3, p.zoom, p.pan.0, p.pan.1,
            );
            g.pop_clip();
            if crate::verification_run()
                && std::env::var_os("KASATERM_TEST_IMAGE_PREVIEW_IDLE").is_some()
            {
                if let Some(report) = std::env::var_os("KASATERM_TEST_IMAGE_PREVIEW") {
                    let report = PathBuf::from(report).with_extension("idle.json");
                    let _ = std::fs::write(report, serde_json::to_vec(&serde_json::json!({"loaded":true,"drawn":g.drawn_image_clipped_rects(&key),"dimensions":image.received})).unwrap());
                }
            }
        }
    }
}
impl Preview {
    fn clamp(&mut self) {
        if let Some(i) = &self.pixels {
            self.pan = clamp_pan(
                self.pan,
                self.area,
                (i.width, i.height),
                self.scale,
                self.zoom,
            );
        }
    }
    fn zoom_by(&mut self, factor: f32, anchor: Point) {
        let Some(i) = &self.pixels else { return };
        let max = 32.0_f32.max(
            i.received.0 as f32 / i.width as f32 / fit(self.area, (i.width, i.height), self.scale),
        );
        let next = (self.zoom * factor).clamp(1.0, max);
        self.pan = zoom_anchor(self.pan, next / self.zoom, self.area, anchor);
        self.zoom = next;
        self.clamp();
    }
}
fn load(path: &str) -> anyhow::Result<Pixels> {
    let mut encoded = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut encoded)?;
    anyhow::ensure!(encoded.len() <= 64 * 1024 * 1024, "이미지가 64MiB를 넘어요");
    let (image, format) = decode(&encoded)?;
    let received = (image.width(), image.height());
    let image = if received.0 > MAX_IMAGE_EDGE || received.1 > MAX_IMAGE_EDGE {
        image.resize(
            MAX_IMAGE_EDGE,
            MAX_IMAGE_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Ok(Pixels {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        received,
        encoded,
        extension: format
            .extensions_str()
            .first()
            .unwrap_or(&"png")
            .to_string(),
    })
}
fn decode(encoded: &[u8]) -> anyhow::Result<(image::DynamicImage, image::ImageFormat)> {
    let mut reader =
        image::ImageReader::new(std::io::Cursor::new(encoded)).with_guessed_format()?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow::anyhow!("지원하지 않는 그림 형식이에요"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10000);
    limits.max_image_height = Some(10000);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok((reader.decode()?, format))
}
fn copy(image: &Pixels) -> Result<String, String> {
    let original = decode(&image.encoded)
        .map_err(|e| e.to_string())?
        .0
        .to_rgba8();
    if crate::verification_run() {
        let path =
            std::env::var_os("KASATERM_TEST_IMAGE_COPY_PATH").ok_or("복사 검증 경로가 없어요")?;
        image::save_buffer(
            Path::new(&path),
            &original,
            original.width(),
            original.height(),
            image::ColorType::Rgba8,
        )
        .map_err(|e| e.to_string())?;
        return Ok("그림 복사 검증 완료".into());
    }
    static CLIPBOARD: std::sync::OnceLock<Mutex<Option<arboard::Clipboard>>> =
        std::sync::OnceLock::new();
    let mut owner = CLIPBOARD.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if owner.is_none() {
        *owner = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
    }
    owner
        .as_mut()
        .unwrap()
        .set_image(arboard::ImageData {
            width: original.width() as usize,
            height: original.height() as usize,
            bytes: std::borrow::Cow::Borrowed(&original),
        })
        .map_err(|e| e.to_string())?;
    Ok("그림을 복사했어요".into())
}
fn save(image: &Pixels) -> Result<String, String> {
    let Some(path) = choose_save(&format!("image.{}", image.extension))? else {
        return Ok("저장을 취소했어요".into());
    };
    std::fs::write(&path, &image.encoded).map_err(|e| format!("저장하지 못했어요: {e}"))?;
    Ok("그림을 저장했어요".into())
}
fn choose_save(name: &str) -> Result<Option<PathBuf>, String> {
    if crate::verification_run() {
        return Ok(std::env::var_os("KASATERM_TEST_IMAGE_SAVE_PATH").map(PathBuf::from));
    }
    #[cfg(target_os = "macos")]
    let result = crate::proc::command("/usr/bin/osascript").args(["-e", &format!("try\nPOSIX path of (choose file name with prompt \"이미지 저장\" default name \"{name}\")\non error number -128\nreturn \"\"\nend try")]).output();
    #[cfg(target_os = "windows")]
    let result = crate::proc::command("powershell").args(["-NoProfile", "-STA", "-Command", &format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.SaveFileDialog; $d.FileName = '{name}'; $d.OverwritePrompt = $true; if ($d.ShowDialog() -eq 'OK') {{ [Console]::Write($d.FileName) }}")]).output();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = crate::proc::command("zenity")
        .args([
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            "--filename",
            name,
        ])
        .output();
    let output = result.map_err(|e| format!("저장 창을 열지 못했어요: {e}"))?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err("저장 창을 완료하지 못했어요".into());
    }
    let path = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
    Ok((!path.trim().is_empty()).then(|| Path::new(path.trim()).to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anchor_zoom_keeps_the_point_under_the_cursor() {
        let area = (10.0, 20.0, 600.0, 400.0);
        let pan = (40.0, -30.0);
        let anchor = (410.0, 320.0);
        let next = zoom_anchor(pan, 2.0, area, anchor);
        assert_eq!(next, (-20.0, -160.0));
    }
    #[test]
    fn fit_and_pan_match_retina_renderer_and_clamp_overscroll() {
        let area = (0.0, 0.0, 400.0, 300.0);
        assert_eq!(fit(area, (1600, 800), 2.0), 0.5);
        assert_eq!(
            clamp_pan((999.0, -999.0), area, (1600, 800), 2.0, 2.0),
            (200.0, -50.0)
        );
        assert_eq!(
            clamp_pan((20.0, 20.0), area, (1600, 800), 2.0, 1.0),
            (0.0, 0.0)
        );
    }
    #[test]
    fn drag_and_visible_bounds_are_strict() {
        assert!(!moved((0.0, 0.0), (0.0, 4.0)));
        assert!(moved((0.0, 0.0), (3.0, 3.0)));
        assert!(contains((10.0, 30.0, 20.0, 10.0), (10.0, 30.0)));
        assert!(!contains((10.0, 30.0, 20.0, 10.0), (10.0, 29.0)));
        assert!(!contains((10.0, 30.0, 20.0, 10.0), (30.0, 30.0)));
    }
}
