use std::f32::consts::FRAC_PI_2;
use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::{Quat, Vec3, vec3};

use crate::roster::DeviceKind;

pub const STAND_TOP: f32 = 0.46;
pub const DEVICE_YAW: f32 = -0.22;

const SOLID: f32 = 0.0;
const LIGHT: f32 = 1.0;
const FLOOR: f32 = 2.0;
const WALL: f32 = 3.0;
const SCREEN: f32 = 4.0;
const SHADOW: f32 = 5.0;
const OUTLINE: f32 = 6.0;
const STAND_TOP_MATERIAL: f32 = 7.0;
const SILVER: [u8; 3] = [177, 197, 216];
const DARK_METAL: [u8; 3] = [65, 82, 103];
const GLASS: [u8; 3] = [17, 27, 44];
const CYAN: [u8; 3] = [115, 225, 249];

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub point: [f32; 3],
    pub normal: [f32; 3],
    pub material: [f32; 4],
}

pub struct Model {
    pub body: Range<u32>,
    pub outline: Range<u32>,
}

pub struct Meshes {
    pub vertices: Vec<Vertex>,
    pub floor: Range<u32>,
    pub wall: Range<u32>,
    pub stand: Range<u32>,
    pub models: [Model; 3],
}

#[derive(Clone, Copy)]
pub struct Body {
    pub center: Vec3,
    pub size: Vec3,
    corner: f32,
    bevel: f32,
    color: [u8; 3],
}

const fn body(
    center: [f32; 3], size: [f32; 3], corner: f32, bevel: f32, color: [u8; 3],
) -> Body {
    Body {
        center: Vec3::from_array(center),
        size: Vec3::from_array(size),
        corner,
        bevel,
        color,
    }
}

const MINI: [Body; 1] = [
    body([0.0, 0.30, 0.0], [1.55, 0.58, 1.55], 0.21, 0.037, SILVER),
];
const LAPTOP: [Body; 2] = [
    body([0.0, 0.075, 0.02], [1.96, 0.12, 1.36], 0.095, 0.019, SILVER),
    body([0.0, 0.67, -0.59], [1.96, 1.12, 0.087], 0.028, 0.017, SILVER),
];
const DESKTOP: [Body; 3] = [
    body([0.74, 0.68, 0.13], [0.58, 1.33, 1.01], 0.050, 0.026, DARK_METAL),
    body([-0.36, 0.94, -0.16], [1.47, 0.89, 0.106], 0.037, 0.017, DARK_METAL),
    body([-0.36, 0.30, -0.16], [0.16, 0.49, 0.13], 0.030, 0.012, DARK_METAL),
];

pub fn bodies(kind: DeviceKind) -> &'static [Body] {
    match kind {
        DeviceKind::Mini => &MINI,
        DeviceKind::Laptop => &LAPTOP,
        DeviceKind::Desktop => &DESKTOP,
    }
}

#[derive(Default)]
struct Mesh {
    vertices: Vec<Vertex>,
    rotation: Quat,
}

impl Mesh {
    fn vertex(&mut self, p: Vec3, n: Vec3, color: [u8; 3], mode: f32) {
        self.vertices.push(Vertex {
            point: (self.rotation * p).to_array(),
            normal: (self.rotation * n).to_array(),
            material: [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, mode],
        });
    }

    fn face(&mut self, points: [Vec3; 3], color: [u8; 3], mode: f32) {
        let [a, b, c] = points;
        let normal = (b - a).cross(c - a).normalize();
        for p in points {
            self.vertex(p, normal, color, mode);
        }
    }

    fn quad(&mut self, [a, b, c, d]: [Vec3; 4], color: [u8; 3], mode: f32) {
        self.face([a, b, c], color, mode);
        self.face([a, c, d], color, mode);
    }

    fn slab(&mut self, part: Body, mode: f32) {
        const STEPS: usize = 3;
        const RING: usize = STEPS * 4;
        let half = part.size * 0.5;
        let corner = part.corner.min(half.x.min(half.z) * 0.98);
        let bevel = part.bevel.min(corner * 0.5).min(half.y * 0.5);
        let ring = |height: f32, inset: f32| -> [Vec3; RING] {
            std::array::from_fn(|i| {
                let quadrant = i / STEPS;
                let a = (quadrant as f32 + (i % STEPS) as f32 / (STEPS - 1) as f32) * FRAC_PI_2;
                let (sin, cos) = a.sin_cos();
                let (sx, sz) = [(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)][quadrant];
                part.center + vec3(
                    sx * (half.x - corner) + cos * (corner - inset),
                    height,
                    sz * (half.z - corner) + sin * (corner - inset),
                )
            })
        };
        let rings = [
            ring(-half.y, bevel),
            ring(-half.y + bevel, 0.0),
            ring(half.y - bevel, 0.0),
            ring(half.y, bevel),
        ];
        for i in 0..RING {
            let j = (i + 1) % RING;
            self.face([part.center - Vec3::Y * half.y, rings[0][i], rings[0][j]], part.color, mode);
            self.face([part.center + Vec3::Y * half.y, rings[3][j], rings[3][i]], part.color, mode);
            for pair in rings.windows(2) {
                let [lower, upper] = [&pair[0], &pair[1]];
                self.quad([lower[i], upper[i], upper[j], lower[j]], part.color, mode);
            }
        }
    }

    fn part(&mut self, center: [f32; 3], size: [f32; 3], edge: f32, color: [u8; 3], material: f32) {
        self.slab(body(center, size, edge, edge * 0.4, color), material);
    }

    fn range(&mut self, draw: impl FnOnce(&mut Self)) -> Range<u32> {
        let start = self.vertices.len() as u32;
        draw(self);
        start..self.vertices.len() as u32
    }

    fn mini(&mut self) {
        self.part([0.0, 0.027, 0.0], [1.36, 0.055, 1.36], 0.19, GLASS, SOLID);
        for x in [-0.42, -0.27] {
            self.part([x, 0.205, 0.775], [0.097, 0.032, 0.012], 0.010, GLASS, SOLID);
        }
        self.part([0.41, 0.206, 0.776], [0.036, 0.016, 0.012], 0.006, CYAN, LIGHT);
    }

    fn laptop(&mut self) {
        self.part([0.0, 0.67, -0.543], [1.87, 1.034, 0.017], 0.018, GLASS, SOLID);
        self.part([0.0, 0.668, -0.532], [1.77, 0.935, 0.010], 0.011, [53, 123, 164], SCREEN);
        self.part([0.0, 1.17, -0.53], [0.033, 0.015, 0.012], 0.006, [117, 148, 179], SOLID);
        self.part([0.0, 0.136, -0.09], [1.63, 0.009, 0.56], 0.023, GLASS, SOLID);
        for row in 0..4 {
            for col in 0..13 {
                let x = (col as f32 - 6.0) * 0.117;
                let z = row as f32 * 0.128 - 0.28;
                self.part([x, 0.142, z], [0.093, 0.012, 0.089], 0.015, [79, 98, 118], SOLID);
            }
        }
        self.part([0.0, 0.136, 0.436], [0.66, 0.006, 0.34], 0.034, [133, 157, 180], SOLID);
    }

    fn desktop(&mut self) {
        self.part([-0.36, 0.055, -0.09], [0.65, 0.08, 0.42], 0.035, DARK_METAL, SOLID);
        self.part([-0.36, 0.94, -0.104], [1.38, 0.80, 0.015], 0.015, GLASS, SOLID);
        self.part([-0.36, 0.945, -0.094], [1.29, 0.712, 0.010], 0.010, [60, 105, 180], SCREEN);
        self.part([0.74, 0.68, 0.639], [0.49, 1.21, 0.018], 0.037, [39, 53, 72], SOLID);
        for i in 0..9 {
            self.part([0.74, i as f32 * 0.069 + 0.25, 0.65], [0.32, 0.022, 0.012], 0.007, GLASS, SOLID);
        }
        self.part([0.74, 1.137, 0.65], [0.12, 0.025, 0.013], 0.008, CYAN, LIGHT);
    }
}

impl Meshes {
    pub fn new() -> Self {
        let mut mesh = Mesh::default();
        let floor = mesh.range(|m| m.quad(
            [vec3(-70., 0., -40.), vec3(-70., 0., 40.), vec3(70., 0., 40.), vec3(70., 0., -40.)],
            [30, 41, 58], FLOOR,
        ));
        let wall = mesh.range(|m| {
            m.quad(
                [vec3(-70., 0., 0.), vec3(70., 0., 0.), vec3(70., 22., 0.), vec3(-70., 22., 0.)],
                [18, 28, 45], WALL,
            );
        });
        let stand = mesh.range(|m| {
            m.quad(
                [vec3(-1.82, 0.01, -1.45), vec3(-1.82, 0.01, 1.57), vec3(1.82, 0.01, 1.57), vec3(1.82, 0.01, -1.45)],
                [4, 8, 13], SHADOW,
            );
            m.part([0.0, 0.12, 0.0], [2.40, 0.24, 1.82], 0.12, [32, 44, 61], SOLID);
            m.part([0.0, 0.275, 0.0], [2.57, 0.085, 1.99], 0.145, [41, 61, 86], SOLID);
            m.part([0.0, 0.389, 0.0], [2.62, 0.142, 2.04], 0.15, [73, 88, 108], STAND_TOP_MATERIAL);
            m.part([0.0, 0.305, 1.007], [1.54, 0.016, 0.012], 0.007, [92, 194, 218], LIGHT);
        });
        mesh.rotation = Quat::from_rotation_y(DEVICE_YAW);
        let models = [DeviceKind::Mini, DeviceKind::Laptop, DeviceKind::Desktop].map(|kind| {
            let body = mesh.range(|m| {
                for part in bodies(kind) {
                    m.slab(*part, SOLID);
                }
                match kind {
                    DeviceKind::Mini => m.mini(),
                    DeviceKind::Laptop => m.laptop(),
                    DeviceKind::Desktop => m.desktop(),
                }
            });
            let outline = mesh.range(|m| {
                for part in bodies(kind) {
                    let expanded = Body {
                        size: part.size + Vec3::splat(0.045),
                        color: CYAN,
                        ..*part
                    };
                    m.slab(expanded, OUTLINE);
                }
            });
            Model { body, outline }
        });
        Self { vertices: mesh.vertices, floor, wall, stand, models }
    }
}
