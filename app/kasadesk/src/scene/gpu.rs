use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glam::{Vec3, vec3};
use wgpu::util::DeviceExt;

use super::{Layout, Slot, ROOM_BG, mesh};

const VERTEX_ATTRS: [wgpu::VertexAttribute; 3] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x4];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![3 => Float32x3, 4 => Float32];
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    position: [f32; 3],
    focus: f32,
}

impl Instance {
    fn at(position: Vec3, focus: f32) -> Self {
        Self { position: position.to_array(), focus }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Camera {
    clip_from_world: [[f32; 4]; 4],
    eye_direction: [f32; 4],
}

pub struct RoomGpu {
    solid: wgpu::RenderPipeline,
    outline: wgpu::RenderPipeline,
    camera: wgpu::Buffer,
    binding: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    instances: wgpu::Buffer,
    floor: Range<u32>,
    wall: Range<u32>,
    stand: Range<u32>,
    models: [mesh::Model; 3],
    depth: Option<(wgpu::Extent3d, wgpu::TextureView)>,
}

impl RoomGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("KasaDesk room camera"),
            size: std::mem::size_of::<Camera>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("KasaDesk room shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("room.wgsl").into()),
        });
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("KasaDesk room camera layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("KasaDesk room pipeline layout"),
            bind_group_layouts: &[&camera_layout],
            immediate_size: 0,
        });
        let pipeline = |cull_mode| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
            primitive: wgpu::PrimitiveState { cull_mode: Some(cull_mode), ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("shade"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let solid = pipeline(wgpu::Face::Back);
        let outline = pipeline(wgpu::Face::Front);
        let binding = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("KasaDesk room camera binding"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: camera.as_entire_binding() }],
        });
        let mesh = mesh::Meshes::new();
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("KasaDesk room meshes"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("KasaDesk room instances"),
            size: (std::mem::size_of::<Instance>() * (2 + 2 * super::MAX_SLOTS)) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { solid, outline, camera, binding, vertices, instances, floor: mesh.floor, wall: mesh.wall,
            stand: mesh.stand, models: mesh.models, depth: None }
    }

    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        view: &wgpu::TextureView,
        layout: &Layout,
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
        let depth = &self.depth.as_ref().unwrap().1;
        queue.write_buffer(&self.camera, 0, bytemuck::bytes_of(&Camera {
            clip_from_world: layout.camera.clip_from_world.to_cols_array_2d(),
            eye_direction: layout.camera.eye_direction.extend(0.0).to_array(),
        }));
        let mut data = Vec::with_capacity(2 + 2 * layout.slots.len());
        data.extend([Instance::at(Vec3::ZERO, 0.0), Instance::at(vec3(0.0, 0.0, layout.back_wall), 0.0)]);
        data.extend(layout.slots.iter().map(|s| Instance::at(s.position, s.lift)));
        let mut raised = Vec::new();
        let model_instances = std::array::from_fn::<_, 3, _>(|kind| {
            let start = data.len() as u32;
            for slot in layout.slots.iter().filter(|s| s.kind as usize == kind) {
                if slot.lift > 0.0 {
                    raised.push((kind, data.len() as u32));
                }
                data.push(Instance::at(Slot::device_position(slot), slot.lift));
            }
            start..data.len() as u32
        });
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&data));

        let [r, g, b, _] = ROOM_BG.map(|c| f64::from(c) / 255.0);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("KasaDesk room"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let area = &layout.camera.area;
        let sy = size.height as f32 / layout.window.1;
        pass.set_viewport(0.0, area.top * sy, size.width as f32, area.height * sy, 0.0, 1.0);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        pass.set_pipeline(&self.solid);
        pass.set_bind_group(0, &self.binding, &[]);
        pass.draw(self.floor.clone(), 0..1);
        pass.draw(self.wall.clone(), 1..2);
        pass.draw(self.stand.clone(), 2..2 + layout.slots.len() as u32);
        for (model, instances) in self.models.iter().zip(model_instances) {
            pass.draw(model.body.clone(), instances);
        }
        if !raised.is_empty() {
            // The outline's far faces only show around the depth-tested body and its details.
            pass.set_pipeline(&self.outline);
            for (kind, i) in raised {
                pass.draw(self.models[kind].outline.clone(), i..i + 1);
            }
        }
    }
}
