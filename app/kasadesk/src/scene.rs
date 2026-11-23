mod gpu;
mod mesh;

use std::borrow::Cow;
use std::time::{Duration, Instant};

use glam::{Mat4, Quat, Vec2, Vec3, vec2, vec3};
use kasa_gridview::GridRenderer;
use winit::keyboard::KeyCode;

use crate::roster::{DeviceKind, Machine};

const PAD: f32 = 24.0;
const ROOM_BG: [u8; 4] = [13, 20, 32, 255];
const TEXT: [u8; 4] = [232, 240, 251, 255];
const MUTE: [u8; 4] = [150, 173, 196, 255];
const ACCENT: [u8; 4] = [139, 233, 250, 255];
const COL_STEP: f32 = 3.15;
const MIN_TRACK_PX: f32 = 212.0;
const MIN_LABEL_PX: f32 = 198.0;
const ROW_STEP: f32 = 6.15;
const LABEL_HEIGHT: f32 = 42.0;
const LABEL_OFFSET: Vec3 = vec3(0.0, 0.03, 1.29);
const MAX_COLUMNS: usize = 4;
const MAX_SLOTS: usize = MAX_COLUMNS * 2;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
const LIFT_TIME: f32 = 0.16;
const LIFT_HEIGHT: f32 = 0.26;

type Point = (f32, f32);
type Rect = (f32, f32, f32, f32);

struct Area {
    width: f32,
    top: f32,
    height: f32,
}

impl Area {
    fn new((width, height): Point) -> Self {
        let top = (height * 0.25).min(86.0);
        let bottom = (height * 0.17).min(53.0);
        Self { width, top, height: height - top - bottom }
    }
}

struct Camera {
    area: Area,
    clip_from_world: Mat4,
    world_from_clip: Mat4,
    eye_direction: Vec3,
    pixels_per_unit: f32,
}

impl Camera {
    fn new(area: Area, rows: usize, cols: usize) -> Self {
        let eye = vec3(0.0, 7.8, 10.1);
        let target = vec3(0.0, 0.35, 0.12);
        let eye_direction = eye.normalize();
        let fit_x = (area.width - 36.0).max(1.0) / (cols.saturating_sub(1) as f32 * COL_STEP + 2.98);
        let fit_y = (area.height - 65.0).max(1.0) / (rows.saturating_sub(1) as f32 * ROW_STEP * eye_direction.y + 3.15);
        let pixels_per_unit = fit_x.min(fit_y).min(124.0);
        let extent = vec2(area.width, area.height) / (pixels_per_unit * 2.0);
        let view = Mat4::look_at_rh(target + eye, target, Vec3::Y);
        let projection = Mat4::orthographic_rh(-extent.x, extent.x, -extent.y, extent.y, 0.1, 60.0);
        let clip_from_world = projection * view;
        let world_from_clip = clip_from_world.inverse();
        Self { area, clip_from_world, world_from_clip, eye_direction, pixels_per_unit }
    }

    fn project(&self, world: Vec3) -> Vec2 {
        let ndc = self.clip_from_world.project_point3(world);
        vec2((ndc.x + 1.0) * 0.5 * self.area.width, self.area.top + (1.0 - ndc.y) * 0.5 * self.area.height)
    }

    fn ray(&self, (x, y): Point) -> Option<Ray> {
        let Area { width, top, height } = self.area;
        if x < 0.0 || x >= width || y < top || y >= top + height {
            return None;
        }
        let x = x / width * 2.0 - 1.0;
        let y = 1.0 - (y - top) / height * 2.0;
        let near = self.world_from_clip.project_point3(vec3(x, y, 0.0));
        let far = self.world_from_clip.project_point3(vec3(x, y, 1.0));
        Some(Ray { origin: near, direction: (far - near).normalize() })
    }
}

struct Ray {
    origin: Vec3,
    direction: Vec3,
}

impl Ray {
    fn hit_box(&self, low: Vec3, high: Vec3) -> Option<f32> {
        let mut near: f32 = 0.0;
        let mut far = f32::INFINITY;
        for axis in 0..3 {
            let o = self.origin[axis];
            let d = self.direction[axis];
            if d.abs() < 1e-6 {
                if o < low[axis] || o > high[axis] {
                    return None;
                }
                continue;
            }
            let a = (low[axis] - o) / d;
            let b = (high[axis] - o) / d;
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        (near <= far).then_some(near)
    }
}

struct Slot {
    index: usize,
    kind: DeviceKind,
    position: Vec3,
    lift: f32,
}

impl Slot {
    fn device_position(&self) -> Vec3 {
        self.position + Vec3::Y * (mesh::STAND_TOP + self.lift * LIFT_HEIGHT)
    }

    fn hit(&self, ray: &Ray, keep_hover: bool) -> Option<f32> {
        let local = Quat::from_rotation_y(-mesh::DEVICE_YAW);
        let ray = Ray {
            origin: local * (ray.origin - self.device_position()),
            direction: local * ray.direction,
        };
        mesh::bodies(self.kind).iter().filter_map(|part| {
            let half = part.size * 0.5 + Vec3::splat(0.025);
            // Keep the pointer over a device as it rises away from its resting silhouette.
            let slack = if keep_hover { Vec3::Y * self.lift * LIFT_HEIGHT } else { Vec3::ZERO };
            ray.hit_box(part.center - half - slack, part.center + half)
        }).min_by(f32::total_cmp)
    }
}

struct Layout {
    window: Point,
    camera: Camera,
    back_wall: f32,
    slots: Vec<Slot>,
}

impl Layout {
    fn columns((width, _): Point) -> usize {
        ((width / 232.0) as usize).clamp(1, MAX_COLUMNS)
    }

    fn page_size(window: Point) -> usize {
        let rows = if Area::new(window).height >= 500.0 { 2 } else { 1 };
        rows * Self::columns(window)
    }

    fn new(window: Point, machines: &[Machine], page: usize) -> Self {
        let columns = Self::columns(window);
        let limit = Self::page_size(window);
        let first = page * limit;
        let devices = &machines[first..machines.len().min(first + limit)];
        let rows = devices.len().div_ceil(columns);
        let half_depth = rows.saturating_sub(1) as f32 * ROW_STEP * 0.5;
        let camera = Camera::new(Area::new(window), rows, columns.min(devices.len()));
        let column_step = COL_STEP.max(MIN_TRACK_PX / camera.pixels_per_unit);
        let slots = devices.chunks(columns).enumerate().flat_map(|(row, devices)| {
            let x_center = devices.len().saturating_sub(1) as f32 / 2.0;
            devices.iter().enumerate().map(move |(col, device)| Slot {
                index: first + row * columns + col,
                kind: device.kind,
                position: vec3((col as f32 - x_center) * column_step, 0.0, row as f32 * ROW_STEP - half_depth),
                lift: 0.0,
            })
        }).collect();
        Self { window, camera, back_wall: -half_depth - 1.74, slots }
    }

    fn hit(&self, pointer: Point, hovered: Option<usize>) -> Option<usize> {
        let ray = self.camera.ray(pointer)?;
        self.slots.iter().filter_map(|slot| {
            slot.hit(&ray, Some(slot.index) == hovered).map(|distance| (distance, slot.index))
        }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, index)| index)
    }
}

#[derive(Default)]
pub struct Room {
    layout: Option<Layout>,
    gpu: Option<gpu::RoomGpu>,
    pointer: Option<Point>,
    hovered: Option<usize>,
    keyboard: Option<String>,
    selected: Option<usize>,
    page: usize,
    animated_at: Option<Instant>,
}

impl Room {
    pub fn reflow(&mut self, window: Point, machines: &[Machine]) {
        let window = (window.0.max(1.0), window.1.max(1.0));
        let limit = Layout::page_size(window);
        let key = self.keyboard.as_ref().and_then(|name| machines.iter().position(|m| &m.label == name));
        if self.keyboard.is_some() && key.is_none() {
            self.keyboard = None;
        }
        self.page = key.map_or(self.page.min(last_page(machines.len(), limit)), |i| i / limit);
        self.layout = Some(Layout::new(window, machines, self.page));
        self.hovered = self.pointer.and_then(|p| self.layout.as_ref()?.hit(p, None));
        self.select(key.or(self.hovered));
    }

    pub fn suspend(&mut self) {
        self.layout = None;
        self.gpu = None;
        self.pointer = None;
        self.hovered = None;
        self.selected = None;
        self.animated_at = None;
    }

    fn select(&mut self, selected: Option<usize>) -> bool {
        let changed = self.selected != selected;
        self.selected = selected;
        if changed || self.animated_at.is_none() {
            self.animated_at = Some(Instant::now());
        }
        changed
    }

    pub fn pointer(&mut self, point: Option<Point>) -> bool {
        self.pointer = point;
        self.hovered = point.and_then(|p| self.layout.as_ref()?.hit(p, self.hovered));
        if point.is_some() {
            self.keyboard = None;
            self.select(self.hovered)
        } else if self.keyboard.is_none() {
            self.select(None)
        } else {
            false
        }
    }

    pub fn rescale_pointer(&mut self, ratio: f32) {
        self.pointer = self.pointer.map(|(x, y)| (x * ratio, y * ratio));
    }

    pub fn pointer_on_device(&self) -> bool {
        self.hovered.is_some()
    }

    pub fn hit(&self) -> Option<usize> {
        self.layout.as_ref()?.hit(self.pointer?, self.hovered)
    }

    pub fn keyboard(&mut self, code: KeyCode, machines: &[Machine]) -> bool {
        let Some(layout) = &self.layout else { return false };
        if machines.is_empty() {
            return false;
        }
        let columns = Layout::columns(layout.window);
        let limit = Layout::page_size(layout.window);
        let first = self.page * limit;
        let window = layout.window;
        let found = match (self.selected, code) {
            (None, KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown) => first,
            (Some(i), KeyCode::ArrowLeft) => i.saturating_sub(1),
            (Some(i), KeyCode::ArrowRight) => (i + 1).min(machines.len() - 1),
            (Some(i), KeyCode::ArrowUp) => adjacent_row(i, -1, columns, machines.len()),
            (Some(i), KeyCode::ArrowDown) => adjacent_row(i, 1, columns, machines.len()),
            _ => return false,
        };
        self.keyboard = Some(machines[found].label.clone());
        if found / limit != self.page {
            self.reflow(window, machines);
            return true;
        }
        self.select(Some(found))
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    fn paging(&self, total: usize) -> Option<(usize, [Rect; 2])> {
        let window = self.layout.as_ref()?.window;
        let last = last_page(total, Layout::page_size(window));
        (last > 0).then(|| (last, [
            (window.0 - PAD - 114.0, 26.0, 26.0, 28.0),
            (window.0 - PAD - 26.0, 26.0, 26.0, 28.0),
        ]))
    }

    pub fn pointer_on_page(&self, total: usize) -> bool {
        self.page_step(total).is_some()
    }

    fn page_step(&self, total: usize) -> Option<isize> {
        let (last, [prev, next]) = self.paging(total)?;
        let p = self.pointer?;
        if self.page > 0 && contains(prev, p) {
            Some(-1)
        } else if self.page < last && contains(next, p) {
            Some(1)
        } else {
            None
        }
    }

    pub fn click_page(&mut self, machines: &[Machine]) -> bool {
        let Some(step) = self.page_step(machines.len()) else { return false };
        self.turn_page(step, machines)
    }

    pub fn turn_page(&mut self, step: isize, machines: &[Machine]) -> bool {
        let Some(layout) = &self.layout else { return false };
        let window = layout.window;
        let last = last_page(machines.len(), Layout::page_size(window));
        let page = self.page.saturating_add_signed(step).min(last);
        if self.page == page {
            return false;
        }
        self.page = page;
        self.keyboard = None;
        self.reflow(window, machines);
        true
    }

    pub fn next_frame(&self) -> Option<Instant> {
        let at = self.animated_at?;
        let layout = self.layout.as_ref()?;
        layout.slots.iter().any(|s| s.lift != lift_target(self.selected, s.index)).then_some(at + FRAME_INTERVAL)
    }

    fn animate(&mut self, now: Instant) {
        let Some(layout) = &mut self.layout else { return };
        let elapsed = self.animated_at.replace(now).map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32());
        let delta = elapsed / LIFT_TIME;
        for slot in &mut layout.slots {
            let target = lift_target(self.selected, slot.index);
            slot.lift = if target > slot.lift { (slot.lift + delta).min(target) } else { (slot.lift - delta).max(target) };
        }
    }

    pub fn paint(
        &mut self,
        g: &mut GridRenderer,
        machines: &[Machine],
        paths: &[String],
        foot: &str,
        app_alive: bool,
    ) -> anyhow::Result<usize> {
        self.animate(Instant::now());
        let Some(layout) = &self.layout else { return g.render(0.0, true, |_, _, _, _, _, _| {}) };
        let (width, height) = layout.window;
        g.draw_label(PAD, 19.0, "KasaDesk", 24.0, TEXT);
        g.draw_label(PAD, 53.0, "화면을 볼 기기를 고르세요", 13.0, MUTE);
        g.rect(0.0, layout.camera.area.top - 1.0, width, 1.0, [60, 77, 98, 255]);
        let footer_top = layout.camera.area.top + layout.camera.area.height;
        g.rect(0.0, footer_top, width, 1.0, [60, 77, 98, 255]);
        let hint = "방향키로 이동 · Enter로 보기";
        let hint_width = g.measure_label(hint, 11.0);
        let show_hint = !machines.is_empty() && width >= 660.0;
        let foot_width = width - PAD * 2.0 - if show_hint { hint_width + 18.0 } else { 0.0 };
        let foot = fit_label(g, foot, 11.0, foot_width);
        g.draw_label(PAD, height - 31.0, &foot, 11.0, MUTE);
        if show_hint {
            g.draw_label(width - PAD - hint_width, height - 31.0, hint, 11.0, MUTE);
        }

        if machines.is_empty() {
            let text = if app_alive { "카사텀 명부에 기기를 추가하세요" } else { "카사텀을 띄우면 기기 명부를 받아요" };
            draw_center(g, width * 0.5, footer_top - 54.0, text, 14.0, MUTE, width - PAD * 2.0);
        }
        for slot in &layout.slots {
            let anchor = layout.camera.project(slot.position + LABEL_OFFSET);
            let max = (layout.camera.pixels_per_unit * 2.91).max(MIN_LABEL_PX).min(width - 30.0);
            let focus = self.selected == Some(slot.index);
            let border = if focus { [90, 172, 194, 255] } else { [64, 83, 108, 255] };
            g.round_rect_fill(anchor.x - max * 0.5, anchor.y, max, LABEL_HEIGHT, 6.0, border);
            g.round_rect_fill(anchor.x - max * 0.5 + 1.0, anchor.y + 1.0, max - 2.0, LABEL_HEIGHT - 2.0, 5.0, [12, 21, 35, 255]);
            draw_center(g, anchor.x, anchor.y + 5.0, &machines[slot.index].label, 15.0, if focus { ACCENT } else { TEXT }, max - 14.0);
            draw_center(g, anchor.x, anchor.y + 24.0, &paths[slot.index], 11.0, MUTE, max - 14.0);
        }

        if let Some((last, [left, right])) = self.paging(machines.len()) {
            for ((x, y, w, h), mark, enabled) in [(left, "‹", self.page > 0), (right, "›", self.page < last)] {
                g.round_rect_fill(x, y, w, h, 5.0, [30, 45, 64, 255]);
                draw_center(g, x + w * 0.5, y + 3.0, mark, 23.0, if enabled { TEXT } else { MUTE }, w);
            }
            let pages = format!("{}/{}", self.page + 1, last + 1);
            draw_center(g, (left.0 + right.0 + right.2) * 0.5, left.1 + 8.0, &pages, 12.0, MUTE, 59.0);
        }

        let gpu = &mut self.gpu;
        g.render_with_underlay(0.0, true, |device, queue, encoder, texture, view, format| {
            gpu.get_or_insert_with(|| gpu::RoomGpu::new(device, format))
                .draw(device, queue, encoder, texture, view, layout);
        })
    }
}

fn adjacent_row(index: usize, step: isize, columns: usize, count: usize) -> usize {
    let row = index / columns;
    let Some(next) = row.checked_add_signed(step).filter(|next| *next < count.div_ceil(columns)) else {
        return index;
    };
    let row_start = row * columns;
    let next_start = next * columns;
    let len = (count - row_start).min(columns);
    let next_len = (count - next_start).min(columns);
    let offset = ((index - row_start) as f32 + (next_len as f32 - len as f32) * 0.5).round();
    next_start + (offset as usize).min(next_len - 1)
}

fn last_page(count: usize, limit: usize) -> usize {
    count.saturating_sub(1) / limit
}

fn lift_target(selected: Option<usize>, index: usize) -> f32 {
    if selected == Some(index) { 1.0 } else { 0.0 }
}

fn contains((x, y, w, h): Rect, (px, py): Point) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

fn fit_label<'a>(g: &mut GridRenderer, text: &'a str, size: f32, max: f32) -> Cow<'a, str> {
    if g.measure_label(text, size) <= max {
        return text.into();
    }
    let ellipsis = "…";
    let available = max - g.measure_label(ellipsis, size);
    if available < 0.0 {
        return "".into();
    }
    let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let n = offsets.partition_point(|&i| g.measure_label(&text[..i], size) <= available);
    let end = offsets[n - 1];
    format!("{}{ellipsis}", &text[..end]).into()
}

fn draw_center(g: &mut GridRenderer, x: f32, y: f32, text: &str, size: f32, ink: [u8; 4], max: f32) {
    let label = fit_label(g, text, size, max);
    let w = g.measure_label(&label, size);
    g.draw_label(x - w * 0.5, y, &label, size, ink);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machines(count: usize) -> Vec<Machine> {
        (0..count).map(|i| Machine {
            label: format!("fixture-{i}"),
            kind: [DeviceKind::Mini, DeviceKind::Laptop, DeviceKind::Desktop][i % 3],
            kasanet: serde_json::Value::Null,
            fresh: false,
        }).collect()
    }

    fn point(v: Vec2) -> Point {
        (v.x, v.y)
    }

    #[test]
    fn projected_models_can_be_picked_at_rest_and_while_rising() {
        let devices = machines(8);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        assert!(room.next_frame().is_none());
        let camera = &room.layout.as_ref().unwrap().camera;
        let positions: Vec<_> = room.layout.as_ref().unwrap().slots.iter().map(|slot| {
            let center = mesh::bodies(slot.kind)[0].center;
            point(camera.project(slot.device_position() + Quat::from_rotation_y(mesh::DEVICE_YAW) * center))
        }).collect();
        for (i, position) in positions.into_iter().enumerate() {
            assert!(room.pointer(Some(position)));
            assert_eq!(room.hit(), Some(i));
        }
        let layout = room.layout.as_ref().unwrap();
        let stand = &layout.slots[0];
        let rest = point(layout.camera.project(stand.device_position() + vec3(0.0, 0.03, 0.75)));
        room.pointer(Some(rest));
        assert_eq!(room.hit(), Some(0));
        assert!(room.next_frame().is_some());
        room.animate(Instant::now() + Duration::from_secs(1));
        assert_eq!(room.hit(), Some(0));
        assert!(room.next_frame().is_none());
        assert!(room.pointer(None));
        assert!(room.next_frame().is_some());
        assert!(room.hit().is_none());
        assert_eq!(room.layout.as_ref().unwrap().hit((640.0, 38.0), None), None);
    }

    #[test]
    fn front_row_models_clear_the_back_row_labels() {
        let devices = machines(8);
        let layout = Layout::new((940.0, 639.0), &devices, 0);
        let projected_top = |slot: &Slot| {
            let rotation = Quat::from_rotation_y(mesh::DEVICE_YAW);
            let raised = slot.device_position() + Vec3::Y * LIFT_HEIGHT;
            mesh::bodies(slot.kind).iter().flat_map(|part| {
                [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)]
                    .map(|(x, z)| {
                        let local = part.center + vec3(x, 1.0, z) * part.size * 0.5;
                        layout.camera.project(raised + rotation * local).y
                    })
            }).min_by(f32::total_cmp).unwrap()
        };
        let label_bottom = layout.camera.project(layout.slots[0].position + LABEL_OFFSET).y + LABEL_HEIGHT;
        assert!(layout.slots[4..].iter().all(|slot| projected_top(slot) > label_bottom + 8.0));
    }

    #[test]
    fn keyboard_follows_centered_rows_and_keeps_the_device_on_resize() {
        let devices = machines(14);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        for key in [KeyCode::ArrowRight, KeyCode::ArrowRight, KeyCode::ArrowDown, KeyCode::ArrowDown] {
            assert!(room.keyboard(key, &devices));
        }
        assert_eq!(room.selected(), Some(9));
        assert_eq!(room.page, 1);
        assert!(room.keyboard(KeyCode::ArrowDown, &devices));
        assert_eq!(room.selected(), Some(12));
        assert!(room.keyboard(KeyCode::ArrowUp, &devices));
        assert_eq!(room.selected(), Some(9));
        room.reflow((360.0, 300.0), &devices);
        assert_eq!(room.page, 9);
        assert_eq!(room.selected(), Some(9));
        room.reflow((360.0, 300.0), &devices[..3]);
        assert_eq!(room.page, 2);
        assert_eq!(room.selected(), None);
    }
}
