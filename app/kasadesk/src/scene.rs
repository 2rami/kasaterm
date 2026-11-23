//! 기기 고르기 방 — 아이소메트릭 모형 방 하나에 기기를 제자리에 둔다: 맥북은 책상 위, Windows 는 책상 위 모니터와 밑의 타워,
//! 맥미니는 왼벽 선반. 창밖은 이 컴퓨터 시각의 하늘이다. 기기를 누르면 카메라가 그 화면 앞으로 다가간 뒤(`take_arrival`)
//! 보기 화면으로 넘어가고, 돌아올 때는 거꾸로 빠져나온다(`surface`). 가만히 있을 때는 하늘만 가끔 다시 그린다.

mod gpu;
mod mesh;

use std::borrow::Cow;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glam::{Mat4, Vec2, Vec3, vec2, vec3};
use kasa_gridview::GridRenderer;
use winit::keyboard::KeyCode;

use crate::roster::{DeviceKind, Machine};
use crate::text::Ui;
use mesh::{Model, Plan};

const PAD: f32 = 24.0;
const TOP: f32 = 78.0;
const BOTTOM: f32 = 52.0;
const TEXT: [u8; 4] = [244, 238, 228, 255];
const MUTE: [u8; 4] = [170, 162, 152, 255];
const WARM: [u8; 4] = [255, 208, 150, 255];
const TAG: [u8; 4] = [34, 30, 29, 228];
const LIVE: [u8; 4] = [130, 218, 146, 255];
const IDLE: [u8; 4] = [128, 120, 112, 255];
/// 다가가며 덮는 색 — 보기 화면 바탕(`desk::BG`)과 같다.
const FADE: [u8; 3] = [24, 24, 27];
/// 한 방에 놓는 기기 수 — 넘치면 장을 넘긴다.
const PAGE: usize = 10;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// 가만히 있을 때도 이만큼마다 한 번 그려 구름·하늘빛을 옮긴다.
const IDLE_TICK: Duration = Duration::from_secs(20);
const LIFT_TIME: f32 = 0.16;
const LIFT_HEIGHT: f32 = 0.45;
const DIVE_TIME: f32 = 0.75;
const EYE_DISTANCE: f32 = 80.0;
const HOME_YAW: f32 = 0.64;
const HOME_PITCH: f32 = 0.55;
const LAPTOP_SLOT: f32 = 3.6;
const DESKTOP_SLOT: f32 = 6.0;

type Point = (f32, f32);
type Rect = (f32, f32, f32, f32);

/// 직교 카메라 — 바라보는 곳·방위각·내려다보는 각·화면 반 높이(세계 단위)·가까운 면.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    target: Vec3,
    yaw: f32,
    pitch: f32,
    half: f32,
    near: f32,
}

impl View {
    fn direction(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        vec3(sy * cp, sp, cy * cp)
    }

    fn mix(a: View, b: View, t: f32) -> View {
        View {
            target: a.target.lerp(b.target, t),
            yaw: a.yaw + (b.yaw - a.yaw) * t,
            pitch: a.pitch + (b.pitch - a.pitch) * t,
            // 확대는 배율로 섞어야 고르게 다가간다.
            half: a.half * (b.half / a.half).powf(t),
            // 앞을 가리는 의자 따위는 끝에 가서야 잘라 낸다.
            near: a.near + (b.near - a.near) * t.powi(4),
        }
    }

    fn frame(&self, window: Point) -> Frame {
        let aspect = window.0 / window.1.max(1.0);
        let eye = self.direction();
        let view = Mat4::look_at_rh(self.target + eye * EYE_DISTANCE, self.target, Vec3::Y);
        let projection = Mat4::orthographic_rh(-self.half * aspect, self.half * aspect, -self.half, self.half, self.near, EYE_DISTANCE + 80.0);
        let clip_from_world = projection * view;
        let (day, dusk, lamp) = gpu::daylight();
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| (d.as_secs() % 100_000) as f32);
        Frame { window, clip_from_world, world_from_clip: clip_from_world.inverse(), eye, time, day, dusk, lamp }
    }
}

pub struct Frame {
    window: Point,
    clip_from_world: Mat4,
    world_from_clip: Mat4,
    eye: Vec3,
    time: f32,
    day: f32,
    dusk: f32,
    lamp: f32,
}

impl Frame {
    fn project(&self, world: Vec3) -> Vec2 {
        let ndc = self.clip_from_world.project_point3(world);
        vec2((ndc.x + 1.0) * 0.5 * self.window.0, (1.0 - ndc.y) * 0.5 * self.window.1)
    }

    fn ray(&self, (x, y): Point) -> Option<Ray> {
        let (w, h) = self.window;
        if x < 0.0 || x >= w || y < 0.0 || y >= h {
            return None;
        }
        let (x, y) = (x / w * 2.0 - 1.0, 1.0 - y / h * 2.0);
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
            let (o, d) = (self.origin[axis], self.direction[axis]);
            if d.abs() < 1e-6 {
                if o < low[axis] || o > high[axis] {
                    return None;
                }
                continue;
            }
            let (a, b) = ((low[axis] - o) / d, (high[axis] - o) / d);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
        (near <= far).then_some(near)
    }
}

/// 방에 놓인 기기 하나 — 들림(고르면 1)과 화면 빛(길이 서 있으면 1).
struct Device {
    index: usize,
    lift: f32,
    glow: f32,
}

/// 기기의 모형 한 조각 — Windows 는 모니터(주)와 타워 둘이다.
struct Part {
    device: usize,
    model: Model,
    position: Vec3,
    lifts: bool,
    main: bool,
}

pub struct Layout {
    window: Point,
    plan: Plan,
    home: View,
    devices: Vec<Device>,
    parts: Vec<Part>,
}

impl Layout {
    fn new(window: Point, machines: &[Machine], page: usize) -> Self {
        let first = page * PAGE;
        let page_machines = &machines[first.min(machines.len())..machines.len().min(first + PAGE)];
        let slot = |kind| if kind == DeviceKind::Desktop { DESKTOP_SLOT } else { LAPTOP_SLOT };
        let on_desk: Vec<usize> = (0..page_machines.len()).filter(|&i| page_machines[i].kind != DeviceKind::Mini).collect();
        let minis: Vec<usize> = (0..page_machines.len()).filter(|&i| page_machines[i].kind == DeviceKind::Mini).collect();
        let desk_len = (2.3 + on_desk.iter().map(|&i| slot(page_machines[i].kind)).sum::<f32>() + 1.9).max(9.5);
        let plan = Plan::new(desk_len, minis.len());
        let dz = plan.desk_z();
        let mut parts = Vec::new();
        let mut x = plan.desk_start();
        for &i in &on_desk {
            let width = slot(page_machines[i].kind);
            let cx = x + width / 2.0;
            x += width;
            if page_machines[i].kind == DeviceKind::Desktop {
                parts.push(Part { device: i, model: Model::Monitor, position: vec3(cx, mesh::DESK_Y, dz - 0.3), lifts: true, main: true });
                parts.push(Part { device: i, model: Model::Tower, position: vec3(cx + 1.0, 0.0, dz + 0.4), lifts: false, main: false });
            } else {
                parts.push(Part { device: i, model: Model::Laptop, position: vec3(cx, mesh::DESK_Y, dz + 0.7), lifts: true, main: true });
            }
        }
        for (k, &i) in minis.iter().enumerate() {
            parts.push(Part { device: i, model: Model::Mini, position: plan.shelf_slot(k), lifts: true, main: true });
        }
        let devices = (0..page_machines.len()).map(|i| Device { index: first + i, lift: 0.0, glow: 0.0 }).collect();
        Self { window, home: home_view(&plan, window), plan, devices, parts }
    }

    fn part_center(&self, part: &Part) -> Vec3 {
        let lift = if part.lifts { self.devices[part.device].lift * LIFT_HEIGHT } else { 0.0 };
        part.position + Vec3::Y * lift
    }

    fn main_part(&self, device: usize) -> Option<&Part> {
        self.parts.iter().find(|p| p.device == device && p.main)
    }

    fn device_of(&self, index: usize) -> Option<usize> {
        self.devices.iter().position(|d| d.index == index)
    }

    fn hit(&self, frame: &Frame, pointer: Point, hovered: Option<usize>) -> Option<usize> {
        let ray = frame.ray(pointer)?;
        self.parts.iter().filter_map(|part| {
            let device = &self.devices[part.device];
            let (center, half) = part.model.hull();
            let center = self.part_center(part) + center;
            // 들리는 동안에도 처음 자리를 누른 채 있으면 놓치지 않는다.
            let slack = if hovered == Some(device.index) && part.lifts { Vec3::Y * device.lift * LIFT_HEIGHT } else { Vec3::ZERO };
            ray.hit_box(center - half - slack, center + half).map(|t| (t, device.index))
        }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, index)| index)
    }

    /// 기기마다 이름표 자리(화면 좌표).
    fn anchors(&self, frame: &Frame) -> Vec<(usize, Vec2)> {
        self.devices.iter().enumerate().filter_map(|(d, device)| {
            let part = self.main_part(d)?;
            Some((device.index, frame.project(self.part_center(part) + part.model.top() + Vec3::Y * 0.35)))
        }).collect()
    }

    /// 그 기기 화면 바로 앞.
    fn dive_view(&self, index: usize) -> Option<View> {
        let part = self.main_part(self.device_of(index)?)?;
        let (face, size, yaw, pitch) = part.model.face();
        let aspect = self.window.0 / self.window.1.max(1.0);
        let half = (size.y * 0.5 / 0.62).max(size.x * 0.5 / 0.84 / aspect);
        Some(View { target: self.part_center(part) + face, yaw, pitch, half, near: EYE_DISTANCE - 2.5 })
    }
}

/// 방 전체가 머리띠와 발띠 사이에 들어오게 맞춘 시점.
fn home_view(plan: &Plan, (width, height): Point) -> View {
    let probe = View { target: Vec3::ZERO, yaw: HOME_YAW, pitch: HOME_PITCH, half: 1.0, near: 0.1 };
    let eye = probe.direction();
    let right = Vec3::Y.cross(eye).normalize();
    let up = eye.cross(right);
    let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for corner in plan.corners() {
        let v = vec2(corner.dot(right), corner.dot(up));
        lo = lo.min(v);
        hi = hi.max(v);
    }
    let size = (hi - lo).max(Vec2::splat(1.0));
    let mid = (lo + hi) * 0.5;
    let room = vec2((width - PAD * 2.0).max(80.0), (height - TOP - BOTTOM).max(80.0));
    let k = (room.x / size.x).min(room.y / size.y);
    let shift = (TOP - BOTTOM) * 0.5 / k;
    View { target: right * mid.x + up * (mid.y + shift), half: height / (2.0 * k), ..probe }
}

/// 다가가기 — `progress` 0 은 방, 1 은 화면 앞. `out` 이면 1 에서 0 으로 빠져나온다.
struct Dive {
    index: usize,
    progress: f32,
    out: bool,
}

impl Dive {
    fn eased(&self) -> f32 {
        let t = self.progress;
        if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
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
    painted_at: Option<Instant>,
    dive: Option<Dive>,
    arrived: Option<usize>,
    tags: Vec<(Rect, usize)>,
}

impl Room {
    pub fn reflow(&mut self, window: Point, machines: &[Machine]) {
        let window = (window.0.max(1.0), window.1.max(1.0));
        let key = self.keyboard.as_ref().and_then(|name| machines.iter().position(|m| &m.label == name));
        if self.keyboard.is_some() && key.is_none() {
            self.keyboard = None;
        }
        self.page = key.map_or(self.page.min(last_page(machines.len())), |i| i / PAGE);
        self.layout = Some(Layout::new(window, machines, self.page));
        self.tags.clear();
        if self.dive.as_ref().is_some_and(|d| self.layout.as_ref().and_then(|l| l.device_of(d.index)).is_none()) {
            self.dive = None;
        }
        self.hovered = self.pointer.and_then(|p| self.hit_at(p, None));
        self.select(key.or(self.hovered).or(self.dive.as_ref().map(|d| d.index)));
    }

    pub fn suspend(&mut self) {
        *self = Self { gpu: None, ..Self::default() };
    }

    fn home_frame(&self) -> Option<Frame> {
        let layout = self.layout.as_ref()?;
        Some(layout.home.frame(layout.window))
    }

    fn hit_at(&self, p: Point, hovered: Option<usize>) -> Option<usize> {
        if self.dive.is_some() {
            return None;
        }
        if let Some(&(_, index)) = self.tags.iter().find(|(r, _)| contains(*r, p)) {
            return Some(index);
        }
        self.layout.as_ref()?.hit(&self.home_frame()?, p, hovered)
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
        if self.dive.is_some() {
            return false;
        }
        self.hovered = point.and_then(|p| self.hit_at(p, self.hovered));
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
        self.dive.is_none() && self.hovered.is_some()
    }

    pub fn hit(&self) -> Option<usize> {
        self.hit_at(self.pointer?, self.hovered)
    }

    /// 그 기기로 다가가기 시작한다 — 다 다가가면 `take_arrival` 이 돌려준다.
    pub fn dive(&mut self, index: usize) -> bool {
        if self.dive.is_some() || self.layout.as_ref().and_then(|l| l.device_of(index)).is_none() {
            return false;
        }
        self.dive = Some(Dive { index, progress: 0.0, out: false });
        self.keyboard = None;
        self.select(Some(index));
        self.animated_at = Some(Instant::now());
        true
    }

    /// 보기 화면에서 돌아올 때 — 그 기기 화면 앞에서 방으로 빠져나온다.
    pub fn surface(&mut self, index: usize) {
        if self.layout.as_ref().and_then(|l| l.device_of(index)).is_some() {
            self.dive = Some(Dive { index, progress: 1.0, out: true });
            self.animated_at = Some(Instant::now());
        }
    }

    pub fn take_arrival(&mut self) -> Option<usize> {
        let index = self.arrived.take()?;
        self.dive = None;
        self.select(self.hovered);
        Some(index)
    }

    pub fn keyboard(&mut self, code: KeyCode, machines: &[Machine]) -> bool {
        if self.dive.is_some() || machines.is_empty() {
            return false;
        }
        let Some(frame) = self.home_frame() else { return false };
        let Some(layout) = &self.layout else { return false };
        let step = match code {
            KeyCode::ArrowLeft => vec2(-1.0, 0.0),
            KeyCode::ArrowRight => vec2(1.0, 0.0),
            KeyCode::ArrowUp => vec2(0.0, -1.0),
            KeyCode::ArrowDown => vec2(0.0, 1.0),
            _ => return false,
        };
        let anchors = layout.anchors(&frame);
        let from = self.selected.and_then(|s| anchors.iter().find(|(i, _)| *i == s)).map(|a| a.1);
        let found = match from {
            None => anchors.first().map(|a| a.0),
            Some(from) => anchors.iter().filter(|(i, _)| Some(*i) != self.selected).filter_map(|&(i, p)| {
                let d = p - from;
                let along = d.dot(step);
                (along > 1.0).then(|| (along + (d - step * along).length() * 2.0, i))
            }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|a| a.1),
        };
        let found = found.or_else(|| {
            // 그쪽에 더 없으면 옆 장으로.
            let last = last_page(machines.len());
            match code {
                KeyCode::ArrowRight if self.page < last => Some((self.page + 1) * PAGE),
                KeyCode::ArrowLeft if self.page > 0 => Some(self.page * PAGE - 1),
                _ => None,
            }
        });
        let Some(found) = found else { return false };
        self.keyboard = Some(machines[found].label.clone());
        if found / PAGE != self.page {
            let window = layout.window;
            self.reflow(window, machines);
            return true;
        }
        self.select(Some(found))
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected.filter(|_| self.dive.is_none())
    }

    fn paging(&self, total: usize) -> Option<(usize, [Rect; 2])> {
        let (width, height) = self.layout.as_ref()?.window;
        let last = last_page(total);
        let y = height - BOTTOM - 34.0;
        (last > 0).then(|| (last, [(width * 0.5 - 62.0, y, 30.0, 28.0), (width * 0.5 + 32.0, y, 30.0, 28.0)]))
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
        let page = self.page.saturating_add_signed(step).min(last_page(machines.len()));
        if self.page == page || self.dive.is_some() {
            return false;
        }
        self.page = page;
        self.keyboard = None;
        self.reflow(window, machines);
        true
    }

    pub fn next_frame(&self) -> Option<Instant> {
        let layout = self.layout.as_ref()?;
        let moving = self.dive.as_ref().is_some_and(|d| self.arrived.is_none() && (d.out || d.progress < 1.0))
            || layout.devices.iter().any(|d| d.lift != lift_target(self.selected, d.index));
        if moving {
            return Some(self.animated_at.unwrap_or_else(Instant::now) + FRAME_INTERVAL);
        }
        self.painted_at.map(|at| at + IDLE_TICK)
    }

    fn animate(&mut self, now: Instant) {
        // 창이 멈췄다 깨어나도 다가가기를 건너뛰지 않게 한 번에 0.1초까지만.
        let elapsed = self.animated_at.replace(now).map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32()).min(0.1);
        let Some(layout) = &mut self.layout else { return };
        let delta = elapsed / LIFT_TIME;
        for device in &mut layout.devices {
            let target = lift_target(self.selected, device.index);
            device.lift = if target > device.lift { (device.lift + delta).min(target) } else { (device.lift - delta).max(target) };
        }
        if let Some(dive) = &mut self.dive {
            let step = elapsed / DIVE_TIME;
            if dive.out {
                dive.progress = (dive.progress - step).max(0.0);
                if dive.progress <= 0.0 {
                    self.dive = None;
                }
            } else {
                dive.progress = (dive.progress + step).min(1.0);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &mut self,
        g: &mut GridRenderer,
        ui: &mut Ui,
        machines: &[Machine],
        paths: &[String],
        live: &[bool],
        foot: &str,
        app_alive: bool,
        overlay: impl FnOnce(&mut GridRenderer, &mut Ui, (f32, f32)),
    ) -> anyhow::Result<usize> {
        let now = Instant::now();
        self.animate(now);
        self.painted_at = Some(now);
        let paging = self.paging(machines.len());
        let Some(layout) = &mut self.layout else { return g.render(0.0, true, |_, _, _, _, _, _| {}) };
        for device in &mut layout.devices {
            device.glow = if live.get(device.index).copied().unwrap_or(false) { 1.0 } else { 0.0 };
        }
        let layout = &*layout;
        let (width, height) = layout.window;
        let dive = self.dive.as_ref().map(|d| (d.index, d.eased(), d.progress));
        let view = dive.and_then(|(i, t, _)| Some(View::mix(layout.home, layout.dive_view(i)?, t))).unwrap_or(layout.home);
        let frame = view.frame(layout.window);
        let calm = 1.0 - dive.map_or(0.0, |(_, t, _)| smooth(0.0, 0.3, t));

        let home = layout.home.frame(layout.window);
        let mut tags = Vec::new();
        if calm > 0.0 {
            // 고른 기기 먼저 제자리에, 나머지는 겹치면 위로 비켜 쌓는다 — 이름을 접지 않는다.
            let mut anchors = layout.anchors(&home);
            anchors.sort_by(|a, b| (Some(b.0) == self.selected).cmp(&(Some(a.0) == self.selected)).then(b.1.y.total_cmp(&a.1.y)));
            let mut placed: Vec<(Rect, usize, Vec2, bool)> = Vec::new();
            for &(index, at) in &anchors {
                let focus = self.selected == Some(index);
                let tag = Tag { name: &machines[index].label, path: &paths[index], live: live.get(index).copied().unwrap_or(false) };
                let (w, h) = tag.size(g, ui, focus);
                let mut rect = (at.x - w / 2.0, at.y - h - 8.0, w, h);
                for _ in 0..12 {
                    let Some(top) = placed.iter().filter(|(r, ..)| overlaps(*r, rect)).map(|(r, ..)| r.1).min_by(f32::total_cmp) else { break };
                    rect.1 = top - h - 4.0;
                }
                placed.push((rect, index, at, focus));
            }
            placed.sort_by_key(|p| p.3);
            // 줄을 먼저 다 긋는다 — 위로 비켜 선 이름표의 줄이 아래 이름표 글자를 지우지 않게.
            for &((_, y, _, h), _, at, _) in &placed {
                let stem = at.y - (y + h);
                if stem > 0.0 {
                    g.rect(at.x - 1.0, y + h, 2.0, stem, fade(TAG, calm * 0.8));
                }
            }
            for (rect, index, _, focus) in placed {
                let tag = Tag { name: &machines[index].label, path: &paths[index], live: live.get(index).copied().unwrap_or(false) };
                tag.draw(g, ui, rect, focus, calm);
                tags.push((rect, index));
            }
        }

        ui.draw(g, PAD, 22.0, "KasaDesk", 20.0, fade(TEXT, calm), true);
        ui.draw(g, PAD, 50.0, "화면을 볼 기기를 눌러 주세요", 13.0, fade(MUTE, calm), false);
        if machines.is_empty() {
            let text = if app_alive { "카사텀 명부에 기기를 추가하세요" } else { "카사텀을 띄우거나 로그인하면 기기가 보여요" };
            let desk = home.project(vec3((layout.plan.desk.0 + layout.plan.desk.1) / 2.0, mesh::DESK_Y + 3.0, layout.plan.desk_z()));
            let w = ui.measure(g, text, 14.0, false) + 32.0;
            g.round_rect_fill(desk.x - w / 2.0, desk.y - 18.0, w, 36.0, 18.0, TAG);
            ui.draw(g, desk.x - w / 2.0 + 16.0, desk.y - 9.0, text, 14.0, TEXT, false);
        }
        if let Some((last, [left, right])) = paging {
            for ((x, y, w, h), mark, enabled) in [(left, "‹", self.page > 0), (right, "›", self.page < last)] {
                g.round_rect_fill(x, y, w, h, 14.0, TAG);
                let mw = ui.measure(g, mark, 18.0, true);
                ui.draw(g, x + (w - mw) / 2.0, y + 3.0, mark, 18.0, if enabled { TEXT } else { IDLE }, true);
            }
            let pages = format!("{}/{}", self.page + 1, last + 1);
            let pw = ui.measure(g, &pages, 12.0, false);
            ui.draw(g, width * 0.5 - pw / 2.0, left.1 + 7.0, &pages, 12.0, MUTE, false);
        }
        let hint = "눌러서 보기 · 방향키와 Enter";
        let hint_width = ui.measure(g, hint, 12.0, false);
        let show_hint = !machines.is_empty() && width >= 680.0;
        let foot_width = width - PAD * 2.0 - if show_hint { hint_width + 24.0 } else { 0.0 };
        let foot = fit(g, ui, foot, 12.0, false, foot_width);
        ui.draw(g, PAD, height - 32.0, &foot, 12.0, fade(MUTE, calm), false);
        if show_hint {
            ui.draw(g, width - PAD - hint_width, height - 32.0, hint, 12.0, fade(MUTE, calm), false);
        }
        overlay(g, ui, (width, height));
        if let Some((_, _, progress)) = dive {
            let a = smooth(0.62, 1.0, progress);
            if a > 0.0 {
                g.rect(0.0, 0.0, width, height, [FADE[0], FADE[1], FADE[2], (a * 255.0) as u8]);
            }
        }
        let arrived = dive.and_then(|(i, _, p)| (p >= 1.0 && self.dive.as_ref().is_some_and(|d| !d.out)).then_some(i));
        let gpu = &mut self.gpu;
        let result = g.render_with_underlay(0.0, true, |device, queue, encoder, texture, target, format| {
            gpu.get_or_insert_with(|| gpu::RoomGpu::new(device, format)).draw(device, queue, encoder, texture, target, layout, &frame);
        });
        self.tags = tags;
        if arrived.is_some() {
            self.arrived = arrived;
        }
        result
    }
}

struct Tag<'a> {
    name: &'a str,
    path: &'a str,
    live: bool,
}

impl Tag<'_> {
    const DOT: f32 = 7.0;
    const NAME_MAX: f32 = 200.0;
    const PATH_MAX: f32 = 300.0;

    fn text_size(focus: bool) -> f32 {
        if focus { 14.0 } else { 12.5 }
    }

    fn lines(&self, g: &mut GridRenderer, ui: &mut Ui, focus: bool) -> (String, Option<String>) {
        let name = fit(g, ui, self.name, Self::text_size(focus), focus, Self::NAME_MAX).into_owned();
        let path = focus.then(|| fit(g, ui, self.path, 11.5, false, Self::PATH_MAX).into_owned());
        (name, path)
    }

    /// 이름표 크기 — 평소엔 점과 이름, 고르면 길 상태 한 줄을 더.
    fn size(&self, g: &mut GridRenderer, ui: &mut Ui, focus: bool) -> (f32, f32) {
        let (name, path) = self.lines(g, ui, focus);
        let name_w = ui.measure(g, &name, Self::text_size(focus), focus);
        let path_w = path.as_ref().map_or(0.0, |p| ui.measure(g, p, 11.5, false));
        ((name_w + Self::DOT + 6.0).max(path_w) + 24.0, if focus { 46.0 } else { 26.0 })
    }

    /// 기기 위에 뜨는 이름표.
    fn draw(&self, g: &mut GridRenderer, ui: &mut Ui, (x, y, w, h): Rect, focus: bool, alpha: f32) {
        let bg = fade(TAG, alpha);
        let size = Self::text_size(focus);
        let (name, path) = self.lines(g, ui, focus);
        if focus {
            g.round_rect_fill(x - 1.5, y - 1.5, w + 3.0, h + 3.0, 12.0, fade(WARM, alpha));
        }
        g.round_rect_fill(x, y, w, h, if focus { 11.0 } else { 13.0 }, bg);
        let line = if focus { 7.0 } else { 5.5 };
        let dot = fade(if self.live { LIVE } else { IDLE }, alpha);
        g.round_rect_fill(x + 12.0, y + line + size * 0.6 - Self::DOT / 2.0, Self::DOT, Self::DOT, Self::DOT / 2.0, dot);
        ui.draw(g, x + 12.0 + Self::DOT + 6.0, y + line, &name, size, fade(TEXT, alpha), focus);
        if let Some(path) = path {
            ui.draw(g, x + 12.0, y + 26.0, &path, 11.5, fade(MUTE, alpha), false);
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 + 4.0 && b.0 < a.0 + a.2 + 4.0 && a.1 < b.1 + b.3 + 4.0 && b.1 < a.1 + a.3 + 4.0
}

fn fade(c: [u8; 4], alpha: f32) -> [u8; 4] {
    [c[0], c[1], c[2], (f32::from(c[3]) * alpha.clamp(0.0, 1.0)) as u8]
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn last_page(count: usize) -> usize {
    count.saturating_sub(1) / PAGE
}

fn lift_target(selected: Option<usize>, index: usize) -> f32 {
    if selected == Some(index) { 1.0 } else { 0.0 }
}

fn contains((x, y, w, h): Rect, (px, py): Point) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

fn fit<'a>(g: &mut GridRenderer, ui: &mut Ui, text: &'a str, size: f32, bold: bool, max: f32) -> Cow<'a, str> {
    if ui.measure(g, text, size, bold) <= max {
        return text.into();
    }
    let available = max - ui.measure(g, "…", size, bold);
    if available <= 0.0 {
        return "".into();
    }
    let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).skip(1).collect();
    let n = offsets.partition_point(|&i| ui.measure(g, &text[..i], size, bold) <= available);
    let end = if n == 0 { 0 } else { offsets[n - 1] };
    format!("{}…", &text[..end]).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machines(kinds: &[DeviceKind]) -> Vec<Machine> {
        kinds.iter().enumerate().map(|(i, &kind)| Machine {
            label: format!("fixture-{i}"),
            kind,
            kasanet: serde_json::Value::Null,
            fresh: false,
            account: None,
        }).collect()
    }

    fn mixed(count: usize) -> Vec<Machine> {
        let kinds: Vec<_> = (0..count).map(|i| [DeviceKind::Laptop, DeviceKind::Mini, DeviceKind::Desktop][i % 3]).collect();
        machines(&kinds)
    }

    /// 한 번에 0.1초까지만 움직이니 여러 번 나눠 끝까지.
    fn settle(room: &mut Room) {
        for _ in 0..20 {
            let at = room.animated_at.unwrap_or_else(Instant::now) + Duration::from_millis(100);
            room.animate(at);
        }
    }

    fn center(layout: &Layout, frame: &Frame, index: usize) -> Point {
        let part = layout.main_part(layout.device_of(index).unwrap()).unwrap();
        let p = frame.project(layout.part_center(part) + part.model.hull().0);
        (p.x, p.y)
    }

    #[test]
    fn devices_sit_on_their_furniture_inside_the_window() {
        let devices = mixed(9);
        let layout = Layout::new((1280.0, 800.0), &devices, 0);
        let frame = layout.home.frame(layout.window);
        for part in &layout.parts {
            match part.model {
                Model::Laptop | Model::Monitor => assert_eq!(part.position.y, mesh::DESK_Y),
                Model::Tower => assert_eq!(part.position.y, 0.0),
                Model::Mini => assert!(part.position.y > mesh::DESK_Y && part.position.x < layout.plan.desk.0),
            }
        }
        for corner in layout.plan.corners() {
            let p = frame.project(corner);
            assert!(p.x >= PAD - 1.0 && p.x <= 1280.0 - PAD + 1.0, "{p}");
            assert!(p.y >= TOP - 1.0 && p.y <= 800.0 - BOTTOM + 1.0, "{p}");
        }
    }

    #[test]
    fn every_device_can_be_picked_at_rest_and_while_rising() {
        let devices = mixed(9);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        let frame = room.home_frame().unwrap();
        for i in 0..devices.len() {
            let p = center(room.layout.as_ref().unwrap(), &frame, i);
            room.pointer(Some(p));
            assert_eq!(room.hit(), Some(i), "기기 {i} at {p:?}");
        }
        assert!(room.next_frame().is_some());
        settle(&mut room);
        assert_eq!(room.hit(), Some(8));
        assert!(room.next_frame().is_none());
        assert!(room.pointer(None));
        assert!(room.hit().is_none());
    }

    #[test]
    fn diving_moves_the_camera_to_the_screen_then_reports_arrival() {
        let devices = mixed(4);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        assert!(room.dive(0));
        assert!(room.hit().is_none() && room.selected().is_none());
        assert!(!room.dive(1));
        let layout = room.layout.as_ref().unwrap();
        let end = layout.dive_view(0).unwrap();
        assert!(end.half < layout.home.half / 4.0);
        let mid = View::mix(layout.home, end, 0.5);
        assert!(mid.half < layout.home.half && mid.half > end.half);
        let screen = end.frame(layout.window).project(end.target);
        assert!((screen.x - 640.0).abs() < 1.0 && (screen.y - 400.0).abs() < 1.0);
        room.dive.as_mut().unwrap().progress = 1.0;
        room.arrived = Some(0);
        assert_eq!(room.take_arrival(), Some(0));
        assert!(room.dive.is_none() && room.take_arrival().is_none());
        room.surface(0);
        assert!(room.next_frame().is_some());
        settle(&mut room);
        assert!(room.dive.is_none());
    }

    #[test]
    fn arrows_walk_the_room_by_where_devices_appear() {
        let devices = machines(&[DeviceKind::Laptop, DeviceKind::Laptop, DeviceKind::Mini, DeviceKind::Desktop]);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        assert!(room.keyboard(KeyCode::ArrowRight, &devices));
        assert_eq!(room.selected(), Some(0));
        assert!(room.keyboard(KeyCode::ArrowRight, &devices));
        assert_eq!(room.selected(), Some(1));
        assert!(room.keyboard(KeyCode::ArrowRight, &devices));
        assert_eq!(room.selected(), Some(3));
        assert!(!room.keyboard(KeyCode::ArrowRight, &devices));
        assert!(room.keyboard(KeyCode::ArrowUp, &devices) || room.keyboard(KeyCode::ArrowLeft, &devices));
    }

    #[test]
    fn pages_hold_ten_devices_and_keep_the_keyboard_device_on_resize() {
        let devices = mixed(23);
        let mut room = Room::default();
        room.reflow((1280.0, 800.0), &devices);
        assert_eq!(room.layout.as_ref().unwrap().devices.len(), PAGE);
        assert!(room.turn_page(2, &devices));
        assert_eq!(room.page, 2);
        assert_eq!(room.layout.as_ref().unwrap().devices.len(), 3);
        room.keyboard = Some(devices[12].label.clone());
        room.reflow((600.0, 400.0), &devices);
        assert_eq!(room.page, 1);
        assert_eq!(room.selected(), Some(12));
        room.reflow((600.0, 400.0), &devices[..3]);
        assert_eq!(room.page, 0);
        assert_eq!(room.selected(), None);
    }
}
