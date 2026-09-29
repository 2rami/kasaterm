//! 날씨의 CPU 쪽: 창마다의 젖음(물방울·김서림·아래 고임·와이퍼)과 단추 물방울의 몸.
//! 단위는 논리 px, y 는 아래로. 창 사각형은 매 프레임 앱 상태에서 새로 받고, 상태는
//! 창 key 로 이어 든다 — 창이 옮겨 가도 제 물을 들고 간다.
//!
//! 물방울 움직임은 raindrop-fx(SardineFish, MIT)의 RaindropSimulator/RainDrop 을 옮긴 것이다.

use super::model::{RainAmount, WeatherSettings, WipeMode};

use std::collections::HashMap;

pub(crate) type V2 = [f32; 2];

fn add(a: V2, b: V2) -> V2 {
    [a[0] + b[0], a[1] + b[1]]
}
fn sub(a: V2, b: V2) -> V2 {
    [a[0] - b[0], a[1] - b[1]]
}
fn scale(a: V2, s: f32) -> V2 {
    [a[0] * s, a[1] * s]
}
fn len(a: V2) -> f32 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}
fn smooth(cur: f32, target: f32, rate: f32, dt: f32) -> f32 {
    cur + (target - cur) * (1.0 - (-dt * rate).exp())
}
pub(crate) fn inside(p: V2, r: [f32; 4]) -> bool {
    p[0] >= r[0] && p[1] >= r[1] && p[0] < r[0] + r[2] && p[1] < r[1] + r[3]
}

struct Rng(u64);

impl Rng {
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f()
    }
}

// streak density, streak alpha, fall speed, window-drop rate, ripple rate, droplet rate
pub(crate) const LEVEL: [[f32; 6]; 4] = [
    [0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
    [0.35, 0.55, 0.7, 0.35, 0.35, 0.4],
    [1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
    [2.2, 1.35, 1.3, 3.0, 2.2, 1.8],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaceKind {
    /// 터미널 창: 유리. 물방울·김서림·고임·와이퍼.
    Pane,
    /// 사이드바·옆 판·오른쪽 열: 물웅덩이. 파문.
    Panel,
    /// 머리줄·하단바: 빗줄기와 단추 물방울만.
    Bar,
}

/// 이번 프레임에 앱이 알려 주는 한 자리.
#[derive(Clone, Debug)]
pub(crate) struct Place {
    pub key: u64,
    pub kind: PlaceKind,
    pub rect: [f32; 4],
    pub amount: RainAmount,
    pub focused: bool,
    /// 초점 창의 입력줄: 물방울을 올리지 않는다.
    pub guard: Option<[f32; 4]>,
}

/// 이번 프레임에 그려진 조작 단추 하나.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ButtonSpot {
    pub rect: [f32; 4],
    pub hover: bool,
    pub enabled: bool,
}

pub(crate) struct PlaceState {
    pub kind: PlaceKind,
    /// 비가 안 오는 자리: 맺힌 물이 몇 초 안에 마른다(대상에서 빠진 창이 계속 젖어 있으면
    /// 설정이 안 먹는 것처럼 보인다).
    pub dry: bool,
    pub rect: [f32; 4],
    pub guard: Option<[f32; 4]>,
    pub focused: bool,
    pub cur: [f32; 6],
    pub idle: f32,
    pub wiper: Option<f32>,
    since_wipe: f32,
    pub pool: f32,
    pub wave: f32,
    rain_acc: f32,
    droplet_acc: f32,
    unseen: f32,
}

const WIPE_SECS: f32 = 0.5;
const POOL_MAX: f32 = 16.0;
pub(crate) const VIS: f32 = 0.36;
const AREA_PER_MASS: f32 = VIS * VIS * std::f32::consts::PI / 4.0;

impl PlaceState {
    // The focused pane is rained on like the rest (「초점 창만」 is the default target);
    // using it is what dries it, through the wiper.
    fn stale(&self, rewet: f32) -> f32 {
        0.4 + 0.6 * (self.idle / rewet).clamp(0.0, 1.0)
    }
    pub(crate) fn mist_target(&self, rewet: f32) -> f32 {
        let m = ((self.idle / rewet - 0.1) / 0.9).clamp(0.0, 1.0) * 0.6;
        if self.focused {
            m * 0.4
        } else {
            m
        }
    }
    pub(crate) fn pool_h(&self) -> f32 {
        (self.pool * AREA_PER_MASS * 2.5 / self.rect[2].max(1.0)).min(POOL_MAX)
    }
    pub(crate) fn wiper_y(&self) -> Option<f32> {
        self.wiper.map(|p| self.rect[1] + p * (self.rect[3] - self.pool_h()))
    }
    fn bottom(&self) -> f32 {
        self.rect[1] + self.rect[3] - self.pool_h()
    }
    fn area_frac(&self) -> f32 {
        self.rect[2] * self.rect[3] / (1920.0 * 1080.0)
    }
}

// ------------------------------------------------------------- window drops

const MOTION_INTERVAL: [f32; 2] = [0.1, 0.4];
const X_SHIFTING: [f32; 2] = [0.0, 0.1];
const TRAIL_DENSITY: f32 = 0.2;
const TRAIL_SIZE: [f32; 2] = [0.3, 0.5];
const TRAIL_DISTANCE: [f32; 2] = [20.0, 30.0];
const TRAIL_SPREAD: f32 = 0.6;
const INITIAL_SPREAD: f32 = 0.5;
const SHRINK_RATE: f32 = 0.01;
const VELOCITY_SPREAD: f32 = 0.3;
const EVAPORATE: f32 = 10.0;
const GRAVITY: f32 = 2400.0;
const MAX_SPAWN: f32 = 100.0;
const CELL: f32 = MAX_SPAWN * 0.3;
const MAX_DROPS: usize = 3000;

#[derive(Clone)]
pub(crate) struct Drop {
    pub pos: V2,
    pub place: u64,
    vel: V2,
    spread: V2,
    mass: f32,
    density: f32,
    resistance: f32,
    shifting: f32,
    last_trail: V2,
    next_trail: f32,
    next_motion: f32,
    id: u32,
    parent: u32,
    dead: bool,
}

impl Drop {
    pub(crate) fn size(&self) -> V2 {
        let s = self.mass.max(0.0).sqrt() / self.density;
        [(self.spread[0] + 1.0) * s, (self.spread[1] + 1.0) * s]
    }
    fn merge_distance(&self) -> f32 {
        self.size()[0] * (1.0 + self.spread[0]) * 0.16
    }
    fn radii(&self) -> V2 {
        let s = self.size();
        [s[0] * VIS * 0.5, s[1] * VIS * 0.5]
    }
}

// --------------------------------------------------------------- buttons

pub(crate) struct Btn {
    pub pos: V2,
    home: V2,
    vel: V2,
    pub half: V2,
    pub group: usize,
    pub hover: f32,
    pub press: f32,
    press_v: f32,
    pub inflate: f32,
    inflate_v: f32,
    pub ring_t: f32,
    enabled: bool,
    hovered: bool,
    seen: bool,
}

// ----------------------------------------------------------------- world

/// 이번 프레임의 사건.
#[derive(Default)]
pub(crate) struct Events {
    /// 입력이 들어간 창 key.
    pub typed: Option<u64>,
    pub mouse: Option<V2>,
    pub mouse_down: bool,
}

pub(crate) struct World {
    pub places: HashMap<u64, PlaceState>,
    pub drops: Vec<Drop>,
    pub buttons: Vec<Btn>,
    pub group_k: Vec<f32>,
    /// 이번 프레임에 판에 떨어진 파문: x, y, 반지름(논리 px), 세기.
    pub ripples: Vec<[f32; 4]>,
    /// 이번 프레임에 맺힌 작은 알갱이: x, y, 크기, 창 key.
    pub droplets: Vec<(V2, f32, u64)>,
    pub time: f32,
    focus: Option<u64>,
    rng: Rng,
    next_id: u32,
    grid: Vec<Vec<usize>>,
    grid_dim: (usize, usize),
    was_down: bool,
    pressed: Option<usize>,
}

impl Default for World {
    fn default() -> Self {
        World {
            places: HashMap::new(),
            drops: Vec::new(),
            buttons: Vec::new(),
            group_k: Vec::new(),
            ripples: Vec::new(),
            droplets: Vec::new(),
            time: 0.0,
            focus: None,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            next_id: 1,
            grid: Vec::new(),
            grid_dim: (0, 0),
            was_down: false,
            pressed: None,
        }
    }
}

impl World {
    /// 창 하나를 한 번 닦는다(초점 이동·입력·손으로).
    pub(crate) fn wipe(&mut self, key: u64) {
        if let Some(p) = self.places.get_mut(&key) {
            if p.kind == PlaceKind::Pane && p.wiper.is_none() {
                p.wiper = Some(0.0);
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        *self = World::default();
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn step(
        &mut self,
        dt: f32,
        s: &WeatherSettings,
        moving: bool,
        places: &[Place],
        buttons: &[ButtonSpot],
        ev: &Events,
        canvas: V2,
    ) {
        self.time += dt;
        self.ripples.clear();
        self.droplets.clear();
        let rewet = s.rewet_secs as f32;
        let wind = s.wind();

        // Places: carry state by key, follow the new rect.
        for p in self.places.values_mut() {
            p.unseen += dt;
        }
        let new_focus = places.iter().find(|p| p.focused).map(|p| p.key);
        for p in places {
            let st = self.places.entry(p.key).or_insert_with(|| PlaceState {
                kind: p.kind,
                dry: true,
                rect: p.rect,
                guard: None,
                focused: false,
                cur: LEVEL[0],
                idle: 0.0,
                wiper: None,
                since_wipe: 99.0,
                pool: 0.0,
                wave: 0.0,
                rain_acc: 0.0,
                droplet_acc: 0.0,
                unseen: 0.0,
            });
            st.kind = p.kind;
            st.dry = p.amount == RainAmount::None;
            st.rect = p.rect;
            st.guard = p.guard;
            st.focused = p.focused;
            st.unseen = 0.0;
            let t = LEVEL[p.amount.index()];
            for i in 0..6 {
                st.cur[i] = smooth(st.cur[i], t[i], 1.6, dt);
            }
        }
        self.places.retain(|_, p| p.unseen < 5.0);
        if new_focus != self.focus {
            if let Some(k) = new_focus {
                if let Some(p) = self.places.get_mut(&k) {
                    p.idle = 0.0;
                    if s.wipe == WipeMode::OnFocus {
                        self.wipe(k);
                    }
                }
            }
            self.focus = new_focus;
        }
        if let Some(k) = ev.typed {
            if let Some(p) = self.places.get_mut(&k) {
                p.idle = 0.0;
                if s.wipe == WipeMode::OnInput && p.wiper.is_none() && p.since_wipe > 1.1 {
                    p.wiper = Some(0.0);
                }
            }
        }

        for p in self.places.values_mut() {
            p.idle += dt;
            p.since_wipe += dt;
            p.wave *= (-dt * 1.5).exp();
            let drain = if p.dry { 1.5 } else { 0.03 };
            p.pool = (p.pool * (-drain * dt).exp()).min(POOL_MAX * p.rect[2] / (AREA_PER_MASS * 2.5));
            // The blade holds one frame at the bottom so everything it carried reaches the pool.
            if p.wiper == Some(1.0) {
                p.wiper = None;
                p.since_wipe = 0.0;
                p.wave = (p.wave + 2.5).min(3.0);
            }
            if let Some(w) = p.wiper.as_mut() {
                *w = if moving { (*w + dt / WIPE_SECS).min(1.0) } else { 1.0 };
            }
            let stale = p.stale(rewet);
            match p.kind {
                PlaceKind::Pane if s.effects.drops => {
                    p.rain_acc += p.cur[3] * 10.0 * p.area_frac() * stale * dt;
                }
                PlaceKind::Panel if s.effects.ripples && moving => {
                    p.rain_acc += p.cur[4] * 8.0 * p.area_frac() * if p.focused { 0.15 } else { 1.0 } * dt;
                }
                _ => {}
            }
            if p.kind == PlaceKind::Pane && s.effects.mist {
                p.droplet_acc += p.cur[5] * 500.0 * p.area_frac() * stale * dt;
            }
        }

        // Spawns.
        let keys: Vec<u64> = self.places.keys().copied().collect();
        for k in keys {
            let (kind, rect, guard, cur) = {
                let p = &self.places[&k];
                (p.kind, p.rect, p.guard, p.cur)
            };
            let d = cur[0];
            let (lo, hi) = (30.0 + 8.0 * d, 55.0 + 14.0 * d);
            while self.places[&k].rain_acc >= 1.0 {
                self.places.get_mut(&k).unwrap().rain_acc -= 1.0;
                let at = [self.rng.range(rect[0], rect[0] + rect[2]), self.rng.range(rect[1], rect[1] + rect[3])];
                if guard.is_some_and(|g| inside(at, g)) {
                    continue;
                }
                match kind {
                    PlaceKind::Pane if self.drops.len() < MAX_DROPS => {
                        let size = self.rng.range(lo, hi);
                        let drop = self.new_drop(at, k, size, 1.0);
                        self.drops.push(drop);
                    }
                    PlaceKind::Panel => {
                        let r = self.rng.range(8.0, 13.0);
                        let st = self.rng.range(0.015, 0.035) * (0.8 + 0.2 * d);
                        self.ripples.push([at[0], at[1], r, st]);
                    }
                    _ => {}
                }
            }
            while self.places[&k].droplet_acc >= 1.0 {
                self.places.get_mut(&k).unwrap().droplet_acc -= 1.0;
                let at = [self.rng.range(rect[0], rect[0] + rect[2]), self.rng.range(rect[1], rect[1] + rect[3])];
                if !guard.is_some_and(|g| inside(at, g)) {
                    let size = self.rng.range(10.0, 30.0);
                    self.droplets.push((at, size, k));
                }
            }
        }

        // Pointer rings in panels, jquery.ripples defaults: 20 px trail, 30 px / 0.14 press.
        if s.effects.ripples && moving {
            if let Some(m) = ev.mouse {
                if self.places.values().any(|p| p.kind == PlaceKind::Panel && inside(m, p.rect)) && ev.mouse_down && !self.was_down {
                    self.ripples.push([m[0], m[1], 30.0, 0.14]);
                }
            }
        }

        if s.effects.drops {
            self.step_drops(dt, wind, moving, canvas);
        } else {
            self.drops.clear();
        }
        self.step_buttons(dt, buttons, ev, s.effects.buttons);
        self.was_down = ev.mouse_down;
    }

    fn new_drop(&mut self, pos: V2, place: u64, size: f32, density: f32) -> Drop {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Drop {
            pos,
            place,
            vel: [0.0; 2],
            spread: [INITIAL_SPREAD; 2],
            mass: (size * density).powi(2),
            density,
            resistance: 0.0,
            shifting: 0.0,
            last_trail: pos,
            next_trail: self.rng.range(TRAIL_DISTANCE[0], TRAIL_DISTANCE[1]),
            next_motion: 0.0,
            id,
            parent: 0,
            dead: false,
        }
    }

    fn step_drops(&mut self, dt: f32, wind: f32, moving: bool, canvas: V2) {
        // Drops whose pane went away, or that sit under the input row, go.
        for d in self.drops.iter_mut() {
            match self.places.get(&d.place) {
                Some(p) if p.kind == PlaceKind::Pane => {
                    let r = d.radii();
                    d.pos[0] = d.pos[0].clamp(p.rect[0] + r[0], (p.rect[0] + p.rect[2] - r[0]).max(p.rect[0] + r[0]));
                    if d.pos[1] < p.rect[1] || p.guard.is_some_and(|g| inside(d.pos, g)) {
                        d.dead = true;
                    }
                }
                _ => d.dead = true,
            }
        }
        if !moving {
            self.drops.retain(|d| !d.dead);
            for p in self.places.values_mut() {
                if p.wiper.is_some() {
                    let key_rect = p.rect;
                    self.drops.retain(|d| !inside(d.pos, key_rect));
                }
            }
            return;
        }
        let steps = (dt / 0.03).ceil().max(1.0) as usize;
        for _ in 0..steps {
            let h = dt / steps as f32;
            self.integrate(h, wind);
            let wipes: Vec<(u64, f32)> =
                self.places.iter().filter_map(|(k, p)| p.wiper_y().map(|y| (*k, y))).collect();
            for (k, y) in wipes {
                for d in self.drops.iter_mut().filter(|d| d.place == k && !d.dead) {
                    let ry = d.radii()[1];
                    if d.pos[1] - ry < y {
                        d.pos[1] = y + ry;
                        d.vel = [0.0; 2];
                    }
                }
            }
            self.collide(canvas);
            self.drops.retain(|d| !d.dead);
        }
    }

    fn integrate(&mut self, dt: f32, wind: f32) {
        let mut trails = Vec::new();
        for i in 0..self.drops.len() {
            if self.drops[i].dead {
                continue;
            }
            if self.drops[i].next_motion <= self.time {
                let max_res = MAX_SPAWN * MAX_SPAWN * 4.0;
                let (a, b, c, e) = (
                    self.rng.range(MOTION_INTERVAL[0], MOTION_INTERVAL[1]),
                    self.rng.f(),
                    self.rng.range(-1.0, 1.0),
                    self.rng.range(X_SHIFTING[0], X_SHIFTING[1]),
                );
                let d = &mut self.drops[i];
                d.next_motion = self.time + a;
                d.resistance = b * GRAVITY * max_res;
                d.shifting = c * e + wind * 0.12;
            }
            let d = &mut self.drops[i];
            let Some(p) = self.places.get_mut(&d.place) else { continue };
            d.mass -= EVAPORATE * if p.dry { 400.0 } else { 1.0 } * dt;
            if d.mass <= 0.0 {
                d.dead = true;
                continue;
            }
            let force = GRAVITY * d.mass - d.resistance;
            d.vel[1] = (d.vel[1] + force / d.mass * dt).max(0.0);
            d.vel[0] = d.vel[1].abs() * d.shifting;
            d.pos = add(d.pos, scale(d.vel, dt));
            let by_vel = VELOCITY_SPREAD * 2.0 * (d.vel[1] * 0.005).atan() / std::f32::consts::PI;
            d.spread[1] = d.spread[1].max(by_vel);
            d.spread = scale(d.spread, SHRINK_RATE.powf(dt));
            let size = d.size();
            let r = d.radii();
            d.pos[0] = d.pos[0].clamp(p.rect[0] + r[0], (p.rect[0] + p.rect[2] - r[0]).max(p.rect[0] + r[0]));
            // Sliding into the input row: the row stays dry, the water is gone.
            if p.guard.is_some_and(|g| inside(d.pos, g)) {
                d.dead = true;
                continue;
            }
            if d.pos[1] + r[1] >= p.bottom() {
                p.pool += d.mass;
                p.wave = (p.wave + 0.5 * (d.mass / 3600.0).sqrt()).min(3.0);
                d.dead = true;
                continue;
            }
            let moved = len(sub(d.last_trail, d.pos));
            if moved > d.next_trail && d.mass >= 1000.0 {
                let (vel_y, parent, at, place) = (d.vel[1], d.id, d.pos, d.place);
                let ts = size[0] * self.rng.range(TRAIL_SIZE[0], TRAIL_SIZE[1]);
                let jitter = self.rng.range(-5.0, 5.0);
                let next = self.rng.range(TRAIL_DISTANCE[0], TRAIL_DISTANCE[1]);
                let mut t = self.new_drop(add(at, [jitter, -size[1] / 4.0]), place, ts, TRAIL_DENSITY);
                t.spread = [0.1, vel_y.abs() * 0.01 * TRAIL_SPREAD];
                t.parent = parent;
                let d = &mut self.drops[i];
                d.mass -= t.mass;
                d.last_trail = d.pos;
                d.next_trail = next;
                trails.push(t);
            } else if moved > d.next_trail {
                d.last_trail = d.pos;
            }
        }
        self.drops.extend(trails);
    }

    fn collide(&mut self, canvas: V2) {
        let dim = ((canvas[0] / CELL).ceil() as usize + 1, (canvas[1] / CELL).ceil() as usize + 1);
        if self.grid_dim != dim {
            self.grid = vec![Vec::new(); dim.0 * dim.1];
            self.grid_dim = dim;
        }
        for g in self.grid.iter_mut() {
            g.clear();
        }
        let (gw, gh) = dim;
        let cell_of = |p: V2| ((p[0].max(0.0) / CELL) as usize, (p[1].max(0.0) / CELL) as usize);
        for (i, d) in self.drops.iter().enumerate() {
            let (x, y) = cell_of(d.pos);
            if x < gw && y < gh && !d.dead {
                self.grid[y * gw + x].push(i);
            }
        }
        for i in 0..self.drops.len() {
            if self.drops[i].dead {
                continue;
            }
            let (cx, cy) = cell_of(self.drops[i].pos);
            for gy in cy.saturating_sub(1)..=(cy + 1).min(gh - 1) {
                for gx in cx.saturating_sub(1)..=(cx + 1).min(gw - 1) {
                    for k in 0..self.grid[gy * gw + gx].len() {
                        let j = self.grid[gy * gw + gx][k];
                        if j == i || self.drops[i].dead {
                            continue;
                        }
                        let (a, b) = (&self.drops[i], &self.drops[j]);
                        if b.dead
                            || a.place != b.place
                            || a.parent == b.id
                            || b.parent == a.id
                            || (a.parent != 0 && a.parent == b.parent)
                        {
                            continue;
                        }
                        if len(sub(a.pos, b.pos)) < a.merge_distance() + b.merge_distance() {
                            let (big, small) = if a.mass >= b.mass { (i, j) } else { (j, i) };
                            let s = self.drops[small].clone();
                            let bd = &mut self.drops[big];
                            let m = bd.mass + s.mass;
                            bd.vel = scale(add(scale(bd.vel, bd.mass), scale(s.vel, s.mass)), 1.0 / m);
                            bd.mass = m;
                            self.drops[small].dead = true;
                        }
                    }
                }
            }
        }
    }

    /// Buttons come and go with the screen; each spot keeps its drop by matching the centre.
    fn step_buttons(&mut self, dt: f32, spots: &[ButtonSpot], ev: &Events, on: bool) {
        if !on {
            self.buttons.clear();
            self.group_k.clear();
            return;
        }
        for b in self.buttons.iter_mut() {
            b.seen = false;
        }
        for s in spots {
            let c = [s.rect[0] + s.rect[2] * 0.5, s.rect[1] + s.rect[3] * 0.5];
            let half = [s.rect[2] * 0.5 + 2.0, s.rect[3] * 0.5 + 2.0];
            match self.buttons.iter_mut().find(|b| !b.seen && len(sub(b.home, c)) < 6.0) {
                Some(b) => {
                    b.home = c;
                    b.half = half;
                    b.enabled = s.enabled;
                    b.hovered = s.hover;
                    b.seen = true;
                }
                None => self.buttons.push(Btn {
                    pos: c,
                    home: c,
                    vel: [0.0; 2],
                    half,
                    group: 0,
                    hover: 0.0,
                    press: 0.0,
                    press_v: 0.0,
                    inflate: if s.enabled { 1.0 } else { 0.0 },
                    inflate_v: 0.0,
                    ring_t: -1.0,
                    enabled: s.enabled,
                    hovered: s.hover,
                    seen: true,
                }),
            }
        }
        self.buttons.retain(|b| b.seen);
        self.pressed = self.pressed.filter(|&i| i < self.buttons.len());

        // Neighbours on one row form a group whose drops bridge while it is hovered.
        let n = self.buttons.len();
        let mut group: Vec<usize> = (0..n).collect();
        fn root(g: &mut [usize], mut i: usize) -> usize {
            while g[i] != i {
                g[i] = g[g[i]];
                i = g[i];
            }
            i
        }
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = (&self.buttons[i], &self.buttons[j]);
                let gap_x = (a.home[0] - b.home[0]).abs() - a.half[0] - b.half[0];
                if (a.home[1] - b.home[1]).abs() < a.half[1].min(b.half[1]) && gap_x < 12.0 {
                    let (ra, rb) = (root(&mut group, i), root(&mut group, j));
                    group[ra] = rb;
                }
            }
        }
        let mut ids: Vec<usize> = Vec::new();
        for i in 0..n {
            let r = root(&mut group, i);
            let gid = ids.iter().position(|&x| x == r).unwrap_or_else(|| {
                ids.push(r);
                ids.len() - 1
            });
            self.buttons[i].group = gid;
        }
        self.group_k.resize(ids.len(), 1.5);

        let hovered = self.buttons.iter().position(|b| b.hovered);
        let active_group = hovered.map(|h| self.buttons[h].group);
        if ev.mouse_down && !self.was_down {
            if let Some(h) = hovered {
                self.pressed = Some(h);
                self.buttons[h].ring_t = 0.0;
            }
        }
        if !ev.mouse_down {
            self.pressed = None;
        }
        for (g, k) in self.group_k.iter_mut().enumerate() {
            *k = smooth(*k, if active_group == Some(g) { 14.0 } else { 1.5 }, 6.0, dt);
        }
        let steps = ((dt * 480.0).ceil() as usize).clamp(1, 64);
        let h = dt / steps as f32;
        let hover_home = hovered.map(|i| self.buttons[i].home);
        let pressed = self.pressed;
        for (i, b) in self.buttons.iter_mut().enumerate() {
            let mut target = b.home;
            if let (Some(hp), Some(g)) = (hover_home, active_group) {
                if g == b.group && hovered != Some(i) {
                    let dir = sub(hp, b.home);
                    let dl = len(dir).max(1e-3);
                    target = add(target, scale(dir, (0.3 * dl).min(8.0) / dl));
                }
            }
            if hovered == Some(i) {
                if let Some(m) = ev.mouse {
                    let pull = scale(sub(m, b.home), 0.15);
                    let pl = len(pull).max(1e-3);
                    target = add(target, scale(pull, pl.min(2.5) / pl));
                }
            }
            let press_t = if pressed == Some(i) { 1.0 } else { 0.0 };
            let inflate_t = if b.enabled { 1.0 } else { 0.0 };
            for _ in 0..steps {
                let f = sub(scale(sub(target, b.pos), 300.0), scale(b.vel, 16.0));
                b.vel = add(b.vel, scale(f, h));
                b.pos = add(b.pos, scale(b.vel, h));
                b.press_v += ((press_t - b.press) * 900.0 - b.press_v * 16.0) * h;
                b.press += b.press_v * h;
                b.inflate_v += ((inflate_t - b.inflate) * 220.0 - b.inflate_v * 13.0) * h;
                b.inflate += b.inflate_v * h;
            }
            b.hover = smooth(b.hover, if hovered == Some(i) { 1.0 } else { 0.0 }, 14.0, dt);
            if b.ring_t >= 0.0 {
                b.ring_t += dt;
                if b.ring_t > 1.0 {
                    b.ring_t = -1.0;
                }
            }
        }
        // The shader blends neighbours group by group, so keep each group contiguous.
        let mut order: Vec<usize> = (0..self.buttons.len()).collect();
        order.sort_by_key(|&i| self.buttons[i].group);
        if order.iter().enumerate().any(|(a, &b)| a != b) {
            let pressed_old = self.pressed;
            let mut sorted = Vec::with_capacity(self.buttons.len());
            let mut taken: Vec<Option<Btn>> = self.buttons.drain(..).map(Some).collect();
            for &i in &order {
                sorted.push(taken[i].take().unwrap());
            }
            self.buttons = sorted;
            self.pressed = pressed_old.and_then(|p| order.iter().position(|&i| i == p));
        }
    }

    /// 이 자리에서 비가 멈췄는지(모든 물이 걷혔는지) — 멈췄으면 다시 그릴 필요가 없다.
    pub(crate) fn settled(&self) -> bool {
        self.drops.is_empty()
            && self.places.values().all(|p| p.cur[0] < 0.01 && p.wiper.is_none() && p.pool_h() < 0.3)
            && self.buttons.iter().all(|b| b.press.abs() < 0.01 && b.ring_t < 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weather::model::WeatherTarget;

    fn settings() -> WeatherSettings {
        WeatherSettings { enabled: true, target: WeatherTarget::AllWindows, amount: RainAmount::Downpour, ..Default::default() }
    }

    fn pane(key: u64, x: f32, focused: bool, guard: Option<[f32; 4]>) -> Place {
        Place { key, kind: PlaceKind::Pane, rect: [x, 0.0, 400.0, 600.0], amount: RainAmount::Downpour, focused, guard }
    }

    #[test]
    fn drops_stay_inside_their_own_pane() {
        let mut w = World::default();
        let s = settings();
        let places = [pane(1, 0.0, true, None), pane(2, 400.0, false, None)];
        for _ in 0..600 {
            w.step(1.0 / 60.0, &s, true, &places, &[], &Events::default(), [800.0, 600.0]);
        }
        assert!(!w.drops.is_empty());
        for d in &w.drops {
            let r = places.iter().find(|p| p.key == d.place).unwrap().rect;
            assert!(d.pos[0] >= r[0] && d.pos[0] <= r[0] + r[2], "drop left its pane");
        }
    }

    #[test]
    fn the_input_row_never_holds_a_drop() {
        let mut w = World::default();
        let s = settings();
        let guard = [0.0, 520.0, 400.0, 80.0];
        let places = [pane(1, 0.0, true, Some(guard))];
        for _ in 0..600 {
            w.step(1.0 / 60.0, &s, true, &places, &[], &Events::default(), [400.0, 600.0]);
            assert!(w.drops.iter().all(|d| !inside(d.pos, guard)));
        }
    }

    #[test]
    fn typing_wipes_and_idle_panes_get_wetter() {
        let mut w = World::default();
        let s = settings();
        let places = [pane(1, 0.0, true, None), pane(2, 400.0, false, None)];
        for _ in 0..300 {
            w.step(1.0 / 60.0, &s, true, &places, &[], &Events::default(), [800.0, 600.0]);
        }
        let typed = Events { typed: Some(1), ..Default::default() };
        w.step(1.0 / 60.0, &s, true, &places, &[], &typed, [800.0, 600.0]);
        assert!(w.places[&1].wiper.is_some());
        assert!(w.places[&2].wiper.is_none());
        for _ in 0..60 {
            w.step(1.0 / 60.0, &s, true, &places, &[], &Events::default(), [800.0, 600.0]);
        }
        let count = |k: u64| w.drops.iter().filter(|d| d.place == k).count();
        assert!(count(2) > count(1), "the unused pane should hold more water");
    }

    #[test]
    fn a_pane_out_of_the_rain_dries_in_seconds() {
        let mut w = World::default();
        let s = settings();
        let wet = [pane(1, 0.0, false, None)];
        for _ in 0..600 {
            w.step(1.0 / 60.0, &s, true, &wet, &[], &Events::default(), [400.0, 600.0]);
        }
        assert!(!w.drops.is_empty());
        let dry = [Place { amount: RainAmount::None, ..pane(1, 0.0, false, None) }];
        for _ in 0..(6 * 60) {
            w.step(1.0 / 60.0, &s, true, &dry, &[], &Events::default(), [400.0, 600.0]);
        }
        assert!(w.drops.is_empty(), "{} drops left", w.drops.len());
        assert!(w.places[&1].pool_h() < 0.3);
    }

    #[test]
    fn a_pressed_button_squashes_and_rings() {
        let mut w = World::default();
        let s = settings();
        let spot = ButtonSpot { rect: [10.0, 10.0, 26.0, 26.0], hover: true, enabled: true };
        let down = Events { mouse: Some([23.0, 23.0]), mouse_down: true, ..Default::default() };
        w.step(1.0 / 60.0, &s, true, &[], &[spot], &Events::default(), [100.0, 100.0]);
        for _ in 0..10 {
            w.step(1.0 / 60.0, &s, true, &[], &[spot], &down, [100.0, 100.0]);
        }
        assert!(w.buttons[0].press > 0.5);
        assert!(w.buttons[0].ring_t >= 0.0);
    }
}
