//! 방과 기기 모형 — 모두 둥근 상자(`slab`)를 쌓아 코드로 짓는다(그림 자산 없음). 길이 1 은 12.7cm 쯤이다(맥미니 19.7cm 가 1.55).
//! 방은 배치(`Plan`)가 바뀔 때만 다시 짓고, 기기 모형은 종류마다 한 벌을 두고 자리(인스턴스)만 바꾼다.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec2, Vec3, vec2, vec3};

/// 재질 — 셰이더 `shade` 의 갈래와 같은 번호다.
mod mat {
    pub const SOLID: f32 = 0.0;
    pub const LED: f32 = 1.0;
    pub const FLOOR: f32 = 2.0;
    pub const WALL: f32 = 3.0;
    pub const SCREEN: f32 = 4.0;
    pub const SHADOW: f32 = 5.0;
    pub const OUTLINE: f32 = 6.0;
    pub const SKY: f32 = 7.0;
    pub const RUG: f32 = 8.0;
    pub const SHADE: f32 = 9.0;
    pub const LEAF: f32 = 10.0;
    pub const WOOD: f32 = 11.0;
}

const ALU: [u8; 3] = [200, 204, 210];
const SPACE: [u8; 3] = [92, 96, 104];
const GLASS: [u8; 3] = [22, 24, 30];
const SCREEN_BLUE: [u8; 3] = [104, 160, 226];
const OAK: [u8; 3] = [184, 136, 92];
const OAK_DARK: [u8; 3] = [128, 90, 60];
const FLOOR_WOOD: [u8; 3] = [168, 122, 82];
const PAPER: [u8; 3] = [232, 222, 204];
const TRIM: [u8; 3] = [216, 200, 176];
const FRAME: [u8; 3] = [246, 240, 230];
const RUG: [u8; 3] = [104, 140, 134];
const POT: [u8; 3] = [198, 122, 86];
const LEAF: [u8; 3] = [88, 142, 90];
const LAMP: [u8; 3] = [248, 218, 166];
const TOWER: [u8; 3] = [46, 49, 56];
const BOOKS: [[u8; 3]; 5] = [[176, 74, 68], [72, 110, 158], [214, 176, 96], [96, 140, 104], [150, 110, 160]];

pub const WALL_H: f32 = 13.5;
const WALL_T: f32 = 0.6;
pub const DESK_Y: f32 = 5.8;
const DESK_DEPTH: f32 = 5.4;
const SHELF_DEPTH: f32 = 2.6;
const SHELF_STEP: f32 = 2.6;
const SHELF_FIRST: f32 = 8.2;
pub const SHELF_SLOTS: usize = 5;
const SHELF_SLOT: f32 = 2.0;
const WINDOW_LOW: f32 = DESK_Y + 2.4;
pub const WINDOW_H: f32 = 4.4;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub point: [f32; 3],
    pub normal: [f32; 3],
    pub material: [f32; 4],
}

/// 기기 모형 — 맥북·맥미니·모니터(Windows 책상 위)·타워(Windows 책상 밑).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Laptop,
    Mini,
    Monitor,
    Tower,
}

pub const MODELS: [Model; 4] = [Model::Laptop, Model::Mini, Model::Monitor, Model::Tower];

const LAPTOP_TILT: f32 = 0.22;

impl Model {
    /// 누름 판정·테두리 빛에 쓰는 겉 상자(모형 원점 기준 가운데와 반 크기).
    pub fn hull(self) -> (Vec3, Vec3) {
        match self {
            Model::Laptop => (vec3(0.0, 0.88, -0.17), vec3(1.25, 0.9, 1.05)),
            Model::Mini => (vec3(0.0, 0.2, 0.0), vec3(0.8, 0.22, 0.8)),
            Model::Monitor => (vec3(0.0, 2.0, -0.3), vec3(2.45, 2.0, 0.55)),
            Model::Tower => (vec3(0.0, 1.8, 0.0), vec3(0.85, 1.8, 1.75)),
        }
    }

    /// 누르면 카메라가 바로 보며 다가갈 자리 — (가운데, 화면에 담을 너비·높이, 바라보는 방위각, 내려다보는 각).
    pub fn face(self) -> (Vec3, Vec2, f32, f32) {
        match self {
            Model::Laptop => (vec3(0.0, 0.98, -0.98), vec2(2.3, 1.45), 0.0, LAPTOP_TILT),
            Model::Mini => (vec3(0.3, 0.2, 0.0), vec2(2.4, 1.5), FRAC_PI_2, 0.42),
            Model::Monitor => (vec3(0.0, 2.58, -0.4), vec2(4.6, 2.6), 0.0, 0.06),
            Model::Tower => (vec3(0.0, 1.8, 1.72), vec2(1.8, 3.6), 0.0, 0.2),
        }
    }

    /// 이름표를 띄울 자리(모형 원점 기준) — 겉 상자 위.
    pub fn top(self) -> Vec3 {
        let (center, half) = self.hull();
        vec3(center.x, center.y + half.y, center.z)
    }
}

/// 방 크기와 가구 자리 — 기기 수로 정한다(`scene::Layout`). x 는 -width/2..width/2, z 는 -depth/2..depth/2.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Plan {
    pub width: f32,
    pub depth: f32,
    /// 책상 왼쪽·오른쪽 끝(x).
    pub desk: (f32, f32),
    /// 선반 판 수와 맥미니 수 — 맥미니가 앉지 않은 자리엔 책을 꽂는다.
    pub boards: usize,
    pub minis: usize,
}

impl Plan {
    pub fn new(desk_len: f32, minis: usize) -> Self {
        let width = 3.8 + desk_len + 6.0;
        let left = -width / 2.0;
        let desk = (left + 3.8, left + 3.8 + desk_len);
        Self { width, depth: 16.0, desk, boards: minis.div_ceil(SHELF_SLOTS).max(1), minis }
    }

    pub fn left(&self) -> f32 {
        -self.width / 2.0
    }

    pub fn right(&self) -> f32 {
        self.width / 2.0
    }

    pub fn back(&self) -> f32 {
        -self.depth / 2.0
    }

    pub fn front(&self) -> f32 {
        self.depth / 2.0
    }

    pub fn desk_z(&self) -> f32 {
        self.back() + DESK_DEPTH / 2.0 + 0.3
    }

    /// 책상 위 기기를 놓는 줄의 왼쪽 끝 — 왼쪽엔 책, 오른쪽엔 머그가 있다.
    pub fn desk_start(&self) -> f32 {
        self.desk.0 + 2.3
    }

    /// 창 가운데와 폭.
    pub fn window(&self) -> (Vec3, f32) {
        let (x0, x1) = self.desk;
        let width = (x1 - x0).clamp(8.0, 13.0);
        (vec3((x0 + x1) / 2.0, WINDOW_LOW + WINDOW_H / 2.0, self.back() + 0.05), width)
    }

    /// 러그 가운데(x, z)와 반 크기.
    pub fn rug(&self) -> (Vec2, Vec2) {
        let (x0, x1) = self.desk;
        let center = vec2((x0 + x1) / 2.0 - 0.5, self.desk_z() + DESK_DEPTH / 2.0 + 4.4);
        (center, vec2((self.width * 0.28).max(6.5), 3.0))
    }

    /// 스탠드 갓 아래 — 셰이더의 따뜻한 점광 자리.
    pub fn lamp(&self) -> Vec3 {
        vec3(self.right() - 2.2, 11.0, self.back() + 2.6)
    }

    /// 맥미니 자리(선반 칸 순서).
    pub fn shelf_slot(&self, i: usize) -> Vec3 {
        let board = (i / SHELF_SLOTS).min(self.boards - 1);
        // 앞 칸부터 채운다 — 카메라 쪽 칸이라 뒤 칸의 책이 맥미니를 가리지 않는다.
        let slot = SHELF_SLOTS - 1 - i % SHELF_SLOTS;
        vec3(self.left() + 0.1 + SHELF_DEPTH / 2.0, SHELF_FIRST + board as f32 * SHELF_STEP, self.shelf_z0() + 1.2 + slot as f32 * SHELF_SLOT)
    }

    fn shelf_z0(&self) -> f32 {
        self.back() + 0.8
    }

    /// 카메라에 담을 방의 귀퉁이들.
    pub fn corners(&self) -> [Vec3; 7] {
        let (l, r, b, f) = (self.left() - WALL_T, self.right(), self.back() - WALL_T, self.front());
        [
            vec3(l, -0.8, f), vec3(r, -0.8, f), vec3(r, -0.8, b), vec3(l, -0.8, b),
            vec3(l, WALL_H, b), vec3(r, WALL_H, b), vec3(l, WALL_H, f),
        ]
    }
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<Vertex>,
    /// 반투명 바닥 그림자 — 불투명한 것을 다 그린 뒤 깊이를 쓰지 않고 덧그린다.
    shadows: Vec<Vertex>,
    rotation: Quat,
    offset: Vec3,
}

impl Mesh {
    fn push(&mut self, p: Vec3, n: Vec3, color: [u8; 3], mode: f32) {
        self.vertices.push(Vertex {
            point: (self.offset + self.rotation * p).to_array(),
            normal: (self.rotation * n).to_array(),
            material: [f32::from(color[0]) / 255.0, f32::from(color[1]) / 255.0, f32::from(color[2]) / 255.0, mode],
        });
    }

    fn face(&mut self, [a, b, c]: [Vec3; 3], color: [u8; 3], mode: f32) {
        let normal = (b - a).cross(c - a).normalize_or_zero();
        for p in [a, b, c] {
            self.push(p, normal, color, mode);
        }
    }

    fn quad(&mut self, [a, b, c, d]: [Vec3; 4], color: [u8; 3], mode: f32) {
        self.face([a, b, c], color, mode);
        self.face([a, c, d], color, mode);
    }

    /// 둥근 상자 — 위아래 모서리를 조금 깎은 둥근 사각 기둥.
    fn slab(&mut self, center: Vec3, size: Vec3, corner: f32, color: [u8; 3], mode: f32) {
        const STEPS: usize = 3;
        const RING: usize = STEPS * 4;
        let half = size * 0.5;
        let corner = corner.min(half.x.min(half.z) * 0.98).max(0.001);
        let bevel = (corner * 0.4).min(half.y * 0.5);
        let ring = |height: f32, inset: f32| -> [Vec3; RING] {
            std::array::from_fn(|i| {
                let quadrant = i / STEPS;
                let a = (quadrant as f32 + (i % STEPS) as f32 / (STEPS - 1) as f32) * FRAC_PI_2;
                let (sin, cos) = a.sin_cos();
                let (sx, sz) = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)][quadrant];
                center + vec3(sx * (half.x - corner) + cos * (corner - inset), height, sz * (half.z - corner) + sin * (corner - inset))
            })
        };
        let rings = [ring(-half.y, bevel), ring(-half.y + bevel, 0.0), ring(half.y - bevel, 0.0), ring(half.y, bevel)];
        for i in 0..RING {
            let j = (i + 1) % RING;
            self.face([center - Vec3::Y * half.y, rings[0][i], rings[0][j]], color, mode);
            self.face([center + Vec3::Y * half.y, rings[3][j], rings[3][i]], color, mode);
            for pair in rings.windows(2) {
                let [lower, upper] = [&pair[0], &pair[1]];
                self.quad([lower[i], upper[i], upper[j], lower[j]], color, mode);
            }
        }
    }

    fn part(&mut self, center: [f32; 3], size: [f32; 3], corner: f32, color: [u8; 3], mode: f32) {
        self.slab(Vec3::from_array(center), Vec3::from_array(size), corner, color, mode);
    }

    /// 바닥 그림자 — 법선 자리에 모서리 좌표(-1..1)를 실어 셰이더가 가운데서 멀수록 옅게 그린다.
    fn shadow(&mut self, center: Vec3, (x, z): (f32, f32)) {
        let y = center.y + 0.1;
        let corner = |u: f32, v: f32| (vec3(center.x + u * x, y, center.z + v * z), vec3(u, 0.0, v));
        for (p, uv) in [corner(-1.0, -1.0), corner(-1.0, 1.0), corner(1.0, 1.0), corner(-1.0, -1.0), corner(1.0, 1.0), corner(1.0, -1.0)] {
            self.shadows.push(Vertex { point: (self.offset + self.rotation * p).to_array(), normal: uv.to_array(), material: [0.0, 0.0, 0.0, mat::SHADOW] });
        }
    }

    fn turned(&mut self, offset: Vec3, rotation: Quat, draw: impl FnOnce(&mut Self)) {
        let saved = (self.rotation, self.offset);
        self.offset = saved.1 + saved.0 * offset;
        self.rotation = saved.0 * rotation;
        draw(self);
        (self.rotation, self.offset) = saved;
    }

    fn range(&mut self, draw: impl FnOnce(&mut Self)) -> Range<u32> {
        let start = self.vertices.len() as u32;
        draw(self);
        start..self.vertices.len() as u32
    }

    fn laptop(&mut self) {
        self.part([0.0, 0.07, 0.0], [2.44, 0.13, 1.72], 0.14, ALU, mat::SOLID);
        self.part([0.0, 0.14, -0.12], [2.1, 0.012, 0.82], 0.04, GLASS, mat::SOLID);
        for row in 0..5 {
            for col in 0..13 {
                let x = (col as f32 - 6.0) * 0.155;
                self.part([x, 0.152, -0.45 + row as f32 * 0.165], [0.125, 0.014, 0.12], 0.02, [62, 64, 70], mat::SOLID);
            }
        }
        self.part([0.0, 0.142, 0.52], [0.86, 0.006, 0.5], 0.05, [180, 185, 192], mat::SOLID);
        // 뒤쪽 경첩에서 조금 젖힌 화면.
        self.turned(vec3(0.0, 0.13, -0.84), Quat::from_rotation_x(-LAPTOP_TILT), |m| {
            m.part([0.0, 0.82, 0.0], [2.44, 1.64, 0.07], 0.1, ALU, mat::SOLID);
            m.part([0.0, 0.84, 0.04], [2.3, 1.5, 0.012], 0.06, GLASS, mat::SOLID);
            m.part([0.0, 0.86, 0.05], [2.16, 1.36, 0.01], 0.04, SCREEN_BLUE, mat::SCREEN);
        });
    }

    fn mini(&mut self) {
        self.part([0.0, 0.2, 0.0], [1.55, 0.39, 1.55], 0.24, ALU, mat::SOLID);
        self.part([0.0, 0.012, 0.0], [1.36, 0.024, 1.36], 0.2, GLASS, mat::SOLID);
        self.part([0.52, 0.16, 0.776], [0.05, 0.03, 0.012], 0.01, [140, 232, 172], mat::LED);
        for x in [-0.5, -0.36] {
            self.part([x, 0.16, 0.776], [0.09, 0.035, 0.012], 0.012, GLASS, mat::SOLID);
        }
    }

    fn monitor(&mut self) {
        self.part([0.0, 0.06, -0.3], [1.6, 0.1, 1.0], 0.2, SPACE, mat::SOLID);
        self.part([0.0, 1.0, -0.55], [0.36, 1.9, 0.14], 0.06, SPACE, mat::SOLID);
        self.part([0.0, 2.55, -0.5], [4.8, 2.85, 0.16], 0.1, [32, 33, 38], mat::SOLID);
        self.part([0.0, 2.58, -0.41], [4.5, 2.55, 0.012], 0.03, SCREEN_BLUE, mat::SCREEN);
    }

    fn tower(&mut self) {
        self.part([0.0, 1.8, 0.0], [1.6, 3.5, 3.4], 0.1, TOWER, mat::SOLID);
        self.part([0.0, 1.8, 1.71], [1.3, 3.1, 0.02], 0.06, [38, 40, 46], mat::SOLID);
        for i in 0..10 {
            self.part([0.0, 0.7 + i as f32 * 0.22, 1.725], [1.0, 0.05, 0.012], 0.02, [26, 27, 31], mat::SOLID);
        }
        self.part([0.0, 3.3, 1.73], [0.3, 0.06, 0.012], 0.02, [150, 190, 255], mat::LED);
    }

    fn model(&mut self, model: Model) {
        match model {
            Model::Laptop => self.laptop(),
            Model::Mini => self.mini(),
            Model::Monitor => self.monitor(),
            Model::Tower => self.tower(),
        }
    }

    /// 고른 기기 밑에 까는 따뜻한 빛 웅덩이 — 그림자처럼 모서리 좌표를 법선 자리에 싣는다.
    fn halo(&mut self, model: Model) {
        let (center, half) = model.hull();
        let (x, z) = (half.x * 1.5 + 0.4, half.z * 1.5 + 0.4);
        let y = center.y - half.y + 0.03;
        let corner = |u: f32, v: f32| (vec3(center.x + u * x, y, center.z + v * z), vec3(u, 0.0, v));
        for (p, uv) in [corner(-1.0, -1.0), corner(-1.0, 1.0), corner(1.0, 1.0), corner(-1.0, -1.0), corner(1.0, 1.0), corner(1.0, -1.0)] {
            self.vertices.push(Vertex { point: (self.rotation * p).to_array(), normal: uv.to_array(), material: [1.0, 0.82, 0.58, mat::OUTLINE] });
        }
    }
}

pub struct Meshes {
    pub vertices: Vec<Vertex>,
    pub room: Range<u32>,
    pub shadows: Range<u32>,
    pub models: [(Range<u32>, Range<u32>); 4],
}

impl Meshes {
    pub fn new(plan: &Plan) -> Self {
        let mut m = Mesh::default();
        let room = m.range(|m| room(m, plan));
        let models = MODELS.map(|model| {
            // 맥미니는 선반에서 앞면이 방 쪽(+x)을 보게 돌린다 — 판 위에 줄지어도 서로 가리지 않는다.
            let turn = if model == Model::Mini { Quat::from_rotation_y(FRAC_PI_2) } else { Quat::IDENTITY };
            m.rotation = turn;
            let body = m.range(|m| m.model(model));
            let halo = m.range(|m| m.halo(model));
            m.rotation = Quat::IDENTITY;
            (body, halo)
        });
        let start = m.vertices.len() as u32;
        let shadows = std::mem::take(&mut m.shadows);
        m.vertices.extend(shadows);
        let shadows = start..m.vertices.len() as u32;
        Self { vertices: m.vertices, room, shadows, models }
    }
}

fn room(m: &mut Mesh, p: &Plan) {
    let (l, r, b, f) = (p.left(), p.right(), p.back(), p.front());
    // 바닥판과 벽 — 앞·오른쪽은 잘라 낸 모형 방.
    m.part([0.0, -0.43, (b - WALL_T + f) / 2.0], [p.width + WALL_T * 2.0, 0.8, p.depth + WALL_T], 0.05, OAK_DARK, mat::WOOD);
    m.quad([vec3(l, 0.0, b), vec3(l, 0.0, f), vec3(r, 0.0, f), vec3(r, 0.0, b)], FLOOR_WOOD, mat::FLOOR);
    m.part([(l - WALL_T + r) / 2.0, WALL_H / 2.0, b - WALL_T / 2.0], [r - l + WALL_T, WALL_H, WALL_T], 0.04, PAPER, mat::WALL);
    m.part([l - WALL_T / 2.0, WALL_H / 2.0, (b + f) / 2.0], [WALL_T, WALL_H, f - b], 0.04, PAPER, mat::WALL);
    m.part([(l + r) / 2.0, 0.45, b + 0.12], [r - l, 0.9, 0.24], 0.03, TRIM, mat::SOLID);
    m.part([l + 0.12, 0.45, (b + f) / 2.0], [0.24, 0.9, f - b], 0.03, TRIM, mat::SOLID);

    // 창 — 책상 위 뒷벽. 유리는 하늘 셰이더.
    let (window, ww) = p.window();
    let (wy0, wy1) = (window.y - WINDOW_H / 2.0, window.y + WINDOW_H / 2.0);
    let wx = window.x;
    m.quad(
        [vec3(wx - ww / 2.0, wy0, b + 0.05), vec3(wx + ww / 2.0, wy0, b + 0.05), vec3(wx + ww / 2.0, wy1, b + 0.05), vec3(wx - ww / 2.0, wy1, b + 0.05)],
        [255, 255, 255],
        mat::SKY,
    );
    for (cx, cy, sx, sy) in [
        (wx, wy0, ww + 0.6, 0.45),
        (wx, wy1, ww + 0.6, 0.45),
        (wx - ww / 2.0, window.y, 0.45, WINDOW_H),
        (wx + ww / 2.0, window.y, 0.45, WINDOW_H),
        (wx, window.y, 0.22, WINDOW_H),
    ] {
        m.part([cx, cy, b + 0.2], [sx, sy, 0.35], 0.06, FRAME, mat::SOLID);
    }
    m.part([wx, wy0 - 0.25, b + 0.5], [ww + 1.2, 0.22, 0.9], 0.05, FRAME, mat::SOLID);

    // 책상 — 상판·다리·서랍.
    let (dx0, dx1) = p.desk;
    let (dcx, dz, dw) = ((dx0 + dx1) / 2.0, p.desk_z(), dx1 - dx0);
    m.shadow(vec3(dcx, 0.0, dz), (dw / 2.0 + 0.6, DESK_DEPTH / 2.0 + 0.6));
    m.part([dcx, DESK_Y - 0.17, dz], [dw, 0.34, DESK_DEPTH], 0.12, OAK, mat::WOOD);
    let (zb, zf) = (dz - DESK_DEPTH / 2.0 + 0.4, dz + DESK_DEPTH / 2.0 - 0.4);
    for (x, z) in [(dx0 + 0.4, zb), (dx1 - 0.4, zb), (dx0 + 0.4, zf), (dx1 - 0.4, zf)] {
        m.part([x, (DESK_Y - 0.34) / 2.0, z], [0.36, DESK_Y - 0.34, 0.36], 0.08, OAK_DARK, mat::WOOD);
    }
    m.part([dx0 + 1.9, DESK_Y - 0.7, dz + 0.4], [3.0, 0.7, DESK_DEPTH - 1.2], 0.08, OAK_DARK, mat::WOOD);
    m.part([dx0 + 1.9, DESK_Y - 0.7, dz + DESK_DEPTH / 2.0 - 0.78], [0.7, 0.08, 0.05], 0.02, [214, 204, 190], mat::SOLID);
    // 의자.
    let cx = dcx + dw * 0.12;
    let cz = dz + DESK_DEPTH / 2.0 + 1.9;
    m.shadow(vec3(cx, 0.0, cz), (1.8, 1.8));
    m.part([cx, 3.4, cz], [2.6, 0.4, 2.6], 0.3, [86, 100, 112], mat::SOLID);
    m.part([cx, 5.4, cz + 1.2], [2.6, 3.8, 0.35], 0.3, [86, 100, 112], mat::SOLID);
    m.part([cx, 1.7, cz], [0.3, 3.2, 0.3], 0.1, [60, 62, 68], mat::SOLID);
    m.part([cx, 0.15, cz], [2.2, 0.2, 2.2], 0.5, [60, 62, 68], mat::SOLID);
    // 책상 위 소품 — 책 두 권과 머그.
    m.part([dx1 - 1.0, DESK_Y + 0.42, dz + 1.3], [0.62, 0.84, 0.62], 0.3, [238, 234, 226], mat::SOLID);
    m.part([dx0 + 1.1, DESK_Y + 0.12, dz + 1.2], [1.4, 0.24, 1.9], 0.04, BOOKS[0], mat::SOLID);
    m.part([dx0 + 1.15, DESK_Y + 0.33, dz + 1.18], [1.3, 0.2, 1.8], 0.04, BOOKS[1], mat::SOLID);

    // 선반 — 왼벽을 따라. 맥미니가 없는 칸엔 책.
    let length = SHELF_SLOTS as f32 * SHELF_SLOT + 0.4;
    let z0 = p.shelf_z0();
    let sx = l + SHELF_DEPTH / 2.0 + 0.1;
    for board in 0..p.boards {
        let y = SHELF_FIRST + board as f32 * SHELF_STEP;
        m.part([sx, y - 0.15, z0 + length / 2.0], [SHELF_DEPTH, 0.3, length], 0.06, OAK, mat::WOOD);
        for z in [z0 + 0.6, z0 + length - 0.6] {
            m.part([l + 0.3, y - 0.7, z], [0.4, 0.8, 0.2], 0.04, [74, 72, 76], mat::SOLID);
        }
        for slot in 0..SHELF_SLOTS {
            let i = board * SHELF_SLOTS + slot;
            if i < p.minis {
                continue;
            }
            let at = p.shelf_slot(i);
            for k in 0..(3 + (i * 7) % 2) {
                let h = 1.4 + ((i * 5 + k * 3) % 4) as f32 * 0.2;
                let z = at.z - 0.6 + k as f32 * 0.4;
                m.part([at.x + 0.1, at.y + h / 2.0, z], [1.7, h, 0.36], 0.04, BOOKS[(i + k) % BOOKS.len()], mat::SOLID);
            }
        }
    }

    // 러그.
    let (rc, rh) = p.rug();
    m.part([rc.x, 0.04, rc.y], [rh.x * 2.0, 0.08, rh.y * 2.0], 0.6, RUG, mat::RUG);

    // 스탠드 — 책상 오른쪽 뒤 구석.
    let lamp = p.lamp();
    m.shadow(vec3(lamp.x, 0.0, lamp.z), (1.4, 1.4));
    m.part([lamp.x, 0.12, lamp.z], [1.6, 0.24, 1.6], 0.75, [54, 56, 60], mat::SOLID);
    m.part([lamp.x, lamp.y / 2.0, lamp.z], [0.18, lamp.y, 0.18], 0.08, [54, 56, 60], mat::SOLID);
    m.part([lamp.x, lamp.y + 0.55, lamp.z], [2.2, 1.5, 2.2], 0.9, LAMP, mat::SHADE);
    // 오른쪽 앞 — 낮은 서랍장과 작은 화분.
    let (cx, cz) = (r - 1.6, f - 4.2);
    m.shadow(vec3(cx, 0.0, cz), (1.6, 2.6));
    m.part([cx, 1.6, cz], [2.4, 3.2, 4.4], 0.12, OAK, mat::WOOD);
    for k in 0..2 {
        m.part([cx - 1.21, 0.85 + k as f32 * 1.5, cz], [0.04, 1.2, 4.0], 0.04, OAK_DARK, mat::WOOD);
        m.part([cx - 1.25, 0.85 + k as f32 * 1.5, cz], [0.06, 0.1, 0.8], 0.03, [214, 204, 190], mat::SOLID);
    }
    m.part([cx, 3.65, cz - 0.9], [0.9, 0.9, 0.9], 0.4, POT, mat::SOLID);
    m.part([cx, 4.35, cz - 0.9], [1.2, 0.9, 1.2], 0.55, LEAF, mat::LEAF);
    m.part([cx + 0.1, 3.4, cz + 0.9], [1.3, 0.16, 1.7], 0.04, BOOKS[2], mat::SOLID);

    // 화분 — 왼쪽 앞.
    let pot = vec3(l + 2.2, 0.0, f - 2.4);
    m.shadow(pot, (1.4, 1.4));
    m.part([pot.x, 1.0, pot.z], [1.8, 2.0, 1.8], 0.8, POT, mat::SOLID);
    m.part([pot.x, 2.02, pot.z], [1.6, 0.06, 1.6], 0.7, [72, 54, 42], mat::SOLID);
    for i in 0..7 {
        let a = i as f32 / 7.0 * TAU;
        let lean = 0.5 + 0.12 * (i % 3) as f32;
        m.turned(pot + vec3(0.0, 2.0, 0.0), Quat::from_rotation_y(a) * Quat::from_rotation_x(lean), |m| {
            m.part([0.0, 1.5, 0.0], [0.8, 2.8, 0.12], 0.38, LEAF, mat::LEAF);
        });
    }
}
