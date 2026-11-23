//! 방 그리기 — 창 렌더러의 덧그리기 패스(`render_with_underlay`)에서 깊이 버퍼를 따로 두고 그린다. 방 모형은 배치가 바뀔 때만
//! 다시 올리고, 기기는 모형 한 벌에 자리(인스턴스)만 바꾼다.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use super::mesh::{self, MODELS};
use super::{Frame, Layout};

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![3 => Float32x3, 4 => Float32, 5 => Float32];
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;
/// 모형 방 둘레 — 보기 화면 바탕(`desk::BG`)과 같은 색이라 다가간 뒤 넘어갈 때 이음매가 없다.
const BG: wgpu::Color = wgpu::Color { r: 24.0 / 255.0, g: 24.0 / 255.0, b: 27.0 / 255.0, a: 1.0 };

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    position: [f32; 3],
    lift: f32,
    glow: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Scene {
    clip_from_world: [[f32; 4]; 4],
    eye: [f32; 4],
    params: [f32; 4],
    lamp: [f32; 4],
    window: [f32; 4],
    rug: [f32; 4],
}

pub struct RoomGpu {
    solid: wgpu::RenderPipeline,
    glaze: wgpu::RenderPipeline,
    scene: wgpu::Buffer,
    binding: wgpu::BindGroup,
    meshes: Option<(mesh::Plan, wgpu::Buffer, mesh::Meshes)>,
    instances: Option<(usize, wgpu::Buffer)>,
    depth: Option<(wgpu::Extent3d, wgpu::TextureView)>,
}

impl RoomGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let scene = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("KasaDesk room scene"),
            size: std::mem::size_of::<Scene>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("KasaDesk room shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("room.wgsl").into()),
        });
        let scene_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("KasaDesk room scene layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("KasaDesk room pipeline layout"),
            bind_group_layouts: &[&scene_layout],
            immediate_size: 0,
        });
        let pipeline = |depth_write| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("KasaDesk room pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("project"),
                    buffers: &[
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<mesh::Vertex>() as u64,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &VERTEX_ATTRS,
                        },
                        wgpu::VertexBufferLayout {
                            array_stride: std::mem::size_of::<Instance>() as u64,
                            step_mode: wgpu::VertexStepMode::Instance,
                            attributes: &INSTANCE_ATTRS,
                        },
                    ],
                    compilation_options: Default::default(),
                },
                // 창틀·나뭇잎처럼 얇은 면이 있어 뒷면을 버리지 않는다.
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: depth_write,
                    depth_compare: wgpu::CompareFunction::LessEqual,
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("shade"),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let solid = pipeline(true);
        // 반투명 바닥 그림자·빛 웅덩이 — 깊이를 쓰지 않고 덧그린다.
        let glaze = pipeline(false);
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("KasaDesk room scene binding"),
            layout: &scene_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: scene.as_entire_binding() }],
        });
        Self { solid, glaze, scene, binding, meshes: None, instances: None, depth: None }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        view: &wgpu::TextureView,
        layout: &Layout,
        frame: &Frame,
    ) {
        let size = texture.size();
        if self.depth.as_ref().is_none_or(|(old, _)| *old != size) {
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("KasaDesk room depth"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some((size, depth.create_view(&Default::default())));
        }
        if self.meshes.as_ref().is_none_or(|(plan, _, _)| *plan != layout.plan) {
            let meshes = mesh::Meshes::new(&layout.plan);
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("KasaDesk room meshes"),
                contents: bytemuck::cast_slice(&meshes.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            self.meshes = Some((layout.plan, buffer, meshes));
        }
        let lamp = layout.plan.lamp();
        let (window, window_width) = layout.plan.window();
        let (rug, rug_half) = layout.plan.rug();
        queue.write_buffer(&self.scene, 0, bytemuck::bytes_of(&Scene {
            clip_from_world: frame.clip_from_world.to_cols_array_2d(),
            eye: frame.eye.extend(0.0).to_array(),
            params: [frame.time, frame.day, frame.dusk, frame.lamp],
            lamp: lamp.extend(1.0).to_array(),
            window: window.extend(window_width).to_array(),
            rug: [rug.x, rug.y, rug_half.x, rug_half.y],
        }));
        // 방 한 칸 + 모형 종류마다 줄 세운 기기 자리.
        let mut data = vec![Instance { position: [0.0; 3], lift: 0.0, glow: 0.0 }];
        let mut ranges = Vec::new();
        let mut raised = Vec::new();
        for (kind, model) in MODELS.iter().enumerate() {
            let start = data.len() as u32;
            for (part, device) in layout.parts.iter().filter(|p| p.model == *model).map(|p| (p, &layout.devices[p.device])) {
                if device.lift > 0.01 {
                    raised.push((kind, data.len() as u32));
                }
                let lift = if part.lifts { device.lift } else { 0.0 };
                data.push(Instance { position: part.position.to_array(), lift, glow: device.glow });
            }
            ranges.push(start..data.len() as u32);
        }
        if self.instances.as_ref().is_none_or(|(cap, _)| *cap < data.len()) {
            let cap = data.len().next_power_of_two().max(16);
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("KasaDesk room instances"),
                size: (std::mem::size_of::<Instance>() * cap) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instances = Some((cap, buffer));
        }
        let (Some((_, instances)), Some((_, vertices, meshes)), Some((_, depth))) = (&self.instances, &self.meshes, &self.depth) else { return };
        queue.write_buffer(instances, 0, bytemuck::cast_slice(&data));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("KasaDesk room"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(BG), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_vertex_buffer(1, instances.slice(..));
        pass.set_bind_group(0, &self.binding, &[]);
        pass.set_pipeline(&self.solid);
        pass.draw(meshes.room.clone(), 0..1);
        for ((body, _), range) in meshes.models.iter().zip(&ranges) {
            if !range.is_empty() {
                pass.draw(body.clone(), range.clone());
            }
        }
        pass.set_pipeline(&self.glaze);
        pass.draw(meshes.shadows.clone(), 0..1);
        if !raised.is_empty() {
            for (kind, i) in raised {
                pass.draw(meshes.models[kind].1.clone(), i..i + 1);
            }
        }
    }
}

/// 이 컴퓨터 시각으로 (낮의 밝기, 해 질 녘, 스탠드) — 6시~18시가 낮, 그 앞뒤 한 시간씩 노을.
pub fn daylight() -> (f32, f32, f32) {
    let hour = local_hour();
    let day = smooth(5.5, 7.0, hour) * (1.0 - smooth(17.5, 19.0, hour));
    let dusk = (1.0 - ((hour - 18.2).abs() / 1.0).min(1.0)).max(1.0 - ((hour - 6.2).abs() / 0.8).min(1.0));
    let lamp = 1.0 - day * 0.75;
    (day, dusk, lamp)
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn local_hour() -> f32 {
    if let Some(h) = std::env::var("KASATERM_VIEW_TEST_HOUR").ok().and_then(|v| v.trim().parse::<f32>().ok()) {
        return h;
    }
    #[cfg(unix)]
    {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: localtime_r 는 넘긴 tm 에만 쓴다.
        if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
            return 12.0;
        }
        tm.tm_hour as f32 + tm.tm_min as f32 / 60.0
    }
    #[cfg(windows)]
    {
        // SAFETY: GetLocalTime 은 넘긴 구조체에만 쓴다.
        let st = unsafe {
            let mut st = std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>();
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st);
            st
        };
        f32::from(st.wHour) + f32::from(st.wMinute) / 60.0
    }
}
