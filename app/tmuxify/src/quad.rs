//! Solid-color rectangle renderer (wgpu) used for window frames,
//! title-bar backgrounds, borders, and resize handles. Pixel-space
//! input; one draw call per render() with as many instances as quads.

use anyhow::Result;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct QuadInstance {
    /// (x, y, w, h) in physical pixels, top-left origin.
    pub rect: [f32; 4],
    pub color: [f32; 4],
}

const SHADER: &str = r#"
struct VsIn {
    @builtin(vertex_index) vid: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
};
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

struct Screen { size: vec2<f32>, };
@group(0) @binding(0) var<uniform> screen: Screen;

@vertex
fn vs(in: VsIn) -> VsOut {
    // Corner offsets for two triangles (TL, BL, BR, TL, BR, TR).
    var ox = array<f32, 6>(0.0, 0.0, 1.0, 0.0, 1.0, 1.0);
    var oy = array<f32, 6>(0.0, 1.0, 1.0, 0.0, 1.0, 0.0);
    let px = in.rect.x + ox[in.vid] * in.rect.z;
    let py = in.rect.y + oy[in.vid] * in.rect.w;
    // Convert pixel coords (top-left origin) to NDC (-1..1, y up).
    let ndc_x = (px / screen.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (py / screen.size.y) * 2.0;
    var out: VsOut;
    out.clip_pos = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

pub struct QuadRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    screen_buf: wgpu::Buffer,
    instance_buf: wgpu::Buffer,
    capacity: u64,
}

impl QuadRenderer {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        screen_w: f32,
        screen_h: f32,
    ) -> Result<Self> {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("quad-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("quad-pl"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<QuadInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x4,
                    offset: 16,
                    shader_location: 1,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                compilation_options: Default::default(),
                buffers: &[instance_layout],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let screen_data = [screen_w, screen_h, 0.0, 0.0];
        let screen_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-screen"),
            contents: bytemuck::cast_slice(&screen_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });

        let capacity = 64u64;
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quad-instance"),
            size: capacity * std::mem::size_of::<QuadInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            pipeline,
            bind_group_layout,
            bind_group,
            screen_buf,
            instance_buf,
            capacity,
        })
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, w: f32, h: f32) {
        let screen_data = [w, h, 0.0, 0.0];
        queue.write_buffer(&self.screen_buf, 0, bytemuck::cast_slice(&screen_data));
    }

    /// Upload instances and draw in a single pass. The caller is
    /// responsible for the render pass; we just bind+draw inside it.
    pub fn draw<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pass: &mut wgpu::RenderPass<'a>,
        instances: &[QuadInstance],
    ) {
        if instances.is_empty() {
            return;
        }
        let needed = instances.len() as u64;
        if needed > self.capacity {
            let mut cap = self.capacity;
            while cap < needed {
                cap *= 2;
            }
            self.instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quad-instance"),
                size: cap * std::mem::size_of::<QuadInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.capacity = cap;
        }
        queue.write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buf.slice(..));
        pass.draw(0..6, 0..instances.len() as u32);
    }

    /// Bind group is tied to the (immutable) layout so reusing on
    /// resize means we re-create the bind group only if we replace
    /// the buffer; for now we just update buffer contents in place.
    pub fn _layout(&self) -> &wgpu::BindGroupLayout {
        &self.bind_group_layout
    }
}
