//! wgpu pipeline that draws one quad per cell instance.
//!
//! Caller fills a slice of `CellInstance`, uploads it via
//! `Pipeline::write_instances`, then issues `Pipeline::draw` inside
//! its own render pass. The pipeline owns the WGSL module, the bind
//! group layout, and a growable instance buffer; the atlas and
//! uniforms are passed in per-frame so the pipeline doesn't tie
//! itself to a single atlas instance.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
pub struct CellInstance {
    /// Cell quad in physical pixels: x, y, w, h. Top-left origin.
    pub cell_px: [f32; 4],
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
    pub fg_rgba: [f32; 4],
    /// Bit 0 set = sample the atlas as a verbatim color bitmap (emoji)
    /// instead of multiplying the fg colour through a coverage mask.
    /// `u32` (not bool) so the type is GPU-uploadable and 4-byte aligned.
    pub flags: u32,
    /// Pad the instance back to a 16-byte multiple. bytemuck::Pod forbids
    /// implicit tail padding, so the gap is named.
    pub _pad: [u32; 3],
}

impl CellInstance {
    pub const FLAG_COLOR: u32 = 1;
    /// Bit 1 set = tint an SVG coverage mask (fg.rgb × texel.a) but skip the
    /// WezTerm text gamma/contrast curve. That curve sharpens font stems but
    /// hard-edges thin SVG strokes, which read as jagged pixels on hover.
    pub const FLAG_ICON: u32 = 2;
    /// 채우기 띠(비트 4). 아틀라스를 안 보고 `u.time` 으로 셰이더가 움직이므로, 띠 인스턴스를 한 번 내 두면
    /// CPU 가 프레임마다 다시 짓지 않는다. `uv.x` 가 0..1 가로 위치다. 왼쪽부터 2.4초에 걸쳐 차고 다시
    /// 시작한다 — 「끝이 있고 거기로 가는 중」. 시간으로 채우므로 찬 칸이 실제 진행률은 아니다.
    ///
    /// 끝을 모르는 「도는 중」은 띠가 아니라 테두리가 말한다 — 한 바퀴(`FLAG_EDGE_ORBIT`)·점선(`FLAG_EDGE_DASH`)
    /// (2026-10-07 「프로세스바 걷어내고」, 같은 날 「숨쉬는 거 헷갈리는데 선택이랑. 한 바퀴 도는 거로 하자」).
    pub const FLAG_BAND_FILL: u32 = 16;
    /// 테두리 한 바퀴(비트 2): 빛 조각 하나가 둥근 사각 윤곽을 따라 시계 방향으로 돈다. 머리가 가장 굵고
    /// 진하며 꼬리로 갈수록 가늘고 옅어진다(혜성). 「일하는 중」이 정지한 초점 테두리와 한눈에 갈리게 숨이 아니라
    /// 움직임으로 말한다 — 숨은 가장 진할 때 초점 테두리와 같아 보였다. 모양은 `edge_orbit_flags`.
    pub const FLAG_EDGE_ORBIT: u32 = 4;
    /// 테두리 점선(비트 3): 같은 윤곽을 점선으로 긋고 그 무늬가 천천히 시계 방향으로 흐른다 — 「뒤에서 도는 중」.
    /// 뒤에서 도는 칸은 프레임을 펌프하지 않아 무늬가 자주 멈춰 서는데, 멈춘 점선도 실선(초점)과는 갈린다.
    /// 모양은 `edge_dash_flags`.
    pub const FLAG_EDGE_DASH: u32 = 8;

    /// 두 테두리 갈래 공통: 쿼드 하나가 둥근 사각 윤곽 전체다. uv 가 -1..1 이고 셰이더가 `fwidth` 로 쿼드의 장치
    /// px 크기를 되짚어 가장자리까지의 거리와 둘레 위 자리를 재므로 네 변·모서리가 한 번에 겹침 없이 서고, 움직임은
    /// `u.time` 으로 셰이더가 셈해 CPU 는 위상을 안 센다. 인스턴스 배치를 안 바꾸고 모양을 실을 자리가 플래그 상위
    /// 비트뿐이라 거기 싼다 — 굵기는 장치 px 1/4 단위(6비트, 최대 15.75), 모서리 반지름은 장치 px 1/2 단위(6비트,
    /// 최대 31.5), 주기는 0.5초 단위(4비트, 0.5~7.5), 끝 칸은 1/15 단위 몫(4비트).
    ///
    /// 한 바퀴: 굵기 둘이 머리·꼬리 끝, 주기가 한 바퀴 시간, 몫은 둘레에서 꼬리가 차지하는 만큼. 몫이라 칸
    /// 크기와 상관없이 같은 모양으로 보인다.
    pub fn edge_orbit_flags(thick_head_px: f32, thick_tail_px: f32, radius_px: f32, lap_s: f32, tail: f32) -> u32 {
        Self::FLAG_EDGE_ORBIT | Self::edge_shape_bits(thick_head_px * 4.0, thick_tail_px * 4.0, radius_px, lap_s, tail)
    }

    /// 점선: 굵기, 무늬 한 칸(선+틈) 길이(장치 px 1 단위, 6비트 — 최대 63), 무늬가 한 칸 흐르는 시간, 몫은 한
    /// 칸에서 선이 차지하는 만큼.
    pub fn edge_dash_flags(thick_px: f32, cycle_px: f32, radius_px: f32, step_s: f32, duty: f32) -> u32 {
        Self::FLAG_EDGE_DASH | Self::edge_shape_bits(thick_px * 4.0, cycle_px, radius_px, step_s, duty)
    }

    fn edge_shape_bits(a: f32, b: f32, radius_px: f32, period_s: f32, frac: f32) -> u32 {
        let q = |v: f32, max: u32| (v.round().max(0.0) as u32).min(max);
        q(a, 63) << 6
            | q(b, 63) << 12
            | q(radius_px * 2.0, 63) << 18
            | q(period_s * 2.0, 15).max(1) << 24
            | q(frac * 15.0, 15) << 28
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Uniforms {
    pub screen_px: [f32; 2],
    /// WezTerm-style alpha curve on the glyph coverage mask. >1 boosts
    /// mid-tones (crisper text), 1.0 = passthrough. Defaults to 1.3.
    pub text_gamma: f32,
    /// Post-gamma alpha multiplier. 1.0 = no extra contrast.
    pub text_contrast: f32,
    /// HSL-style saturation multiplier on fg / colored-glyph rgb. 1.0 =
    /// passthrough; bumping (e.g. 1.4) closes the visual gap with
    /// ghostty's punchier reds / greens on the same P3 panel.
    pub color_sat: f32,
    /// 0.0 = pass colours through unchanged (sRGB stays sRGB — what the
    /// default RawHandle/sublayer path does and must do, because the
    /// layer is treated as sRGB by macOS).
    /// 1.0 = apply sugarloaf's sRGB→Display P3 Bradford matrix in linear
    /// space, then re-encode. Combined with a CAMetalLayer that's
    /// actually been tagged DisplayP3 (KASATERM_P3_ROOT path), this
    /// produces the same byte values sugarloaf measures (e.g. byte
    /// (255,0,0) → stored (234,52,35), display shows P3-red).
    pub p3_convert: f32,
    /// Monotonic seconds for GPU-driven animation (the decoration bands).
    /// Rewritten every present; the bar quad itself never re-emits.
    pub time: f32,
    pub _pad: f32,
}

pub struct Pipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u32,
    uniform_buffer: wgpu::Buffer,
}

impl Pipeline {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        initial_instance_capacity: u32,
    ) -> Self {
        Self::with_filtering(device, surface_format, initial_instance_capacity, false)
    }

    /// Same pipeline, but `filterable=true` builds a bind-group layout that
    /// accepts a linear-filtering sampler + filterable texture. The glyph
    /// atlas wants Nearest (crisp bitmaps), but a photographic image scaled
    /// to fit a pane wants bilinear so it doesn't look pixelated — that pass
    /// uses its own Pipeline built with this set to true.
    pub fn with_filtering(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        initial_instance_capacity: u32,
        filterable: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cell-renderer shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let sampler_ty = if filterable {
            wgpu::SamplerBindingType::Filtering
        } else {
            wgpu::SamplerBindingType::NonFiltering
        };
        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("cell-renderer bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(sampler_ty),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cell-renderer pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            immediate_size: 0,
        });

        // Vertex attribute layout mirrors `CellInstance` field order
        // exactly. We stream instances only — no per-vertex buffer —
        // and the WGSL vertex shader expands the quad via the builtin
        // vertex index, so we ship 0 vertex buffers and 1 instance
        // buffer.
        let instance_attrs = [
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 24,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 4,
                format: wgpu::VertexFormat::Uint32,
            },
        ];
        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<CellInstance>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &instance_attrs,
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cell-renderer pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[instance_layout],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cell-renderer instance buffer"),
            size: (initial_instance_capacity as u64)
                * std::mem::size_of::<CellInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("cell-renderer uniform buffer"),
            contents: bytemuck::bytes_of(&Uniforms {
                screen_px: [1.0, 1.0],
                text_gamma: 1.3,
                text_contrast: 1.0,
                color_sat: 1.0,
                p3_convert: 0.0,
                time: 0.0,
                _pad: 0.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            pipeline,
            bind_group_layout,
            instance_buffer,
            instance_capacity: initial_instance_capacity,
            uniform_buffer,
        }
    }

    pub fn make_bind_group(
        &self,
        device: &wgpu::Device,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cell-renderer bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        })
    }

    pub fn write_uniforms(&self, queue: &wgpu::Queue, screen_px: [f32; 2]) {
        self.write_uniforms_full(queue, screen_px, 1.3, 1.0, 1.0, false, 0.0);
    }

    /// Same as `write_uniforms` but lets the host plug in custom text gamma
    /// / contrast / colour saturation. Defaults to WezTerm's 1.3 / 1.0
    /// baseline + neutral saturation when the 2-arg form is used.
    /// `p3_convert` switches on the sRGB→Display P3 Bradford matrix in the
    /// fragment shader — only meaningful when the surface's CAMetalLayer
    /// is actually tagged DisplayP3 (otherwise colours look washed).
    pub fn write_uniforms_full(
        &self,
        queue: &wgpu::Queue,
        screen_px: [f32; 2],
        text_gamma: f32,
        text_contrast: f32,
        color_sat: f32,
        p3_convert: bool,
        time: f32,
    ) {
        let u = Uniforms {
            screen_px,
            text_gamma,
            text_contrast,
            color_sat,
            p3_convert: if p3_convert { 1.0 } else { 0.0 },
            time,
            _pad: 0.0,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&u));
    }

    /// Refresh only the `time` field for per-present GPU animation (the
    /// decoration bands) without re-sending the full uniform block or
    /// re-reading render knobs. Offset-targeted write into the existing buffer.
    pub fn write_time(&self, queue: &wgpu::Queue, time: f32) {
        let off = std::mem::offset_of!(Uniforms, time) as u64;
        queue.write_buffer(&self.uniform_buffer, off, bytemuck::bytes_of(&time));
    }

    pub fn write_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        instances: &[CellInstance],
    ) {
        let needed = instances.len() as u32;
        if needed > self.instance_capacity {
            // Grow geometrically so a burst of new glyphs doesn't
            // trigger an allocation every frame.
            let new_cap = needed.next_power_of_two().max(64);
            self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cell-renderer instance buffer (grown)"),
                size: (new_cap as u64) * std::mem::size_of::<CellInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_cap;
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(instances));
        }
    }

    pub fn draw<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        bind_group: &'a wgpu::BindGroup,
        instance_count: u32,
    ) {
        self.draw_range(pass, bind_group, 0, instance_count);
    }

    /// Draw only `start..end` of the instance buffer. The caller uses this to
    /// interleave other passes (images, icons) between slices of the chrome
    /// pass, so the order things were queued in is the order they end up in.
    pub fn draw_range<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        bind_group: &'a wgpu::BindGroup,
        start: u32,
        end: u32,
    ) {
        if end <= start {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, start..end);
    }

    /// Draw a single instance at `instance_index` with its own bind group.
    /// The image pass uploads every image quad into one buffer, then issues
    /// one of these per image so each can bind a different texture (one
    /// bind group per image) while sharing the instance buffer.
    pub fn draw_at<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        bind_group: &'a wgpu::BindGroup,
        instance_index: u32,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
        pass.draw(0..6, instance_index..instance_index + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::CellInstance;

    #[test]
    fn edge_orbit_flags_pack_without_touching_other_flags() {
        let f = CellInstance::edge_orbit_flags(5.0, 3.0, 4.0, 2.0, 0.2);
        // 아래 여섯 비트에선 한 바퀴 비트만 — 색 글리프·아이콘·점선·채우기 띠를 건드리면 셰이더가 다른 갈래로 샌다.
        assert_eq!(f & 0x3f, CellInstance::FLAG_EDGE_ORBIT);
        assert_eq!((f >> 6) & 0x3f, 20, "머리 5px → 1/4 단위 20");
        assert_eq!((f >> 12) & 0x3f, 12, "꼬리 끝 3px → 12");
        assert_eq!((f >> 18) & 0x3f, 8, "반지름 4px → 1/2 단위 8");
        assert_eq!((f >> 24) & 0xf, 4, "2초 → 0.5초 단위 4");
        assert_eq!((f >> 28) & 0xf, 3, "꼬리 0.2 → 1/15 단위 3");
    }

    #[test]
    fn edge_dash_flags_carry_the_cycle_in_whole_device_px() {
        let f = CellInstance::edge_dash_flags(2.0, 20.0, 4.0, 3.0, 0.6);
        assert_eq!(f & 0x3f, CellInstance::FLAG_EDGE_DASH);
        assert_eq!((f >> 6) & 0x3f, 8, "굵기 2px → 1/4 단위 8");
        assert_eq!((f >> 12) & 0x3f, 20, "한 칸 20px 그대로");
        assert_eq!((f >> 24) & 0xf, 6, "3초 → 6");
        assert_eq!((f >> 28) & 0xf, 9, "선 몫 0.6 → 9");
    }

    #[test]
    fn edge_flags_clamp_instead_of_spilling_into_neighbours() {
        let f = CellInstance::edge_orbit_flags(100.0, 100.0, 100.0, 100.0, 9.0);
        assert_eq!(f & 0x3f, CellInstance::FLAG_EDGE_ORBIT);
        assert_eq!((f >> 6) & 0x3f, 63);
        assert_eq!((f >> 12) & 0x3f, 63);
        assert_eq!((f >> 18) & 0x3f, 63);
        assert_eq!((f >> 24) & 0xf, 15);
        assert_eq!((f >> 28) & 0xf, 15);
        assert_eq!((CellInstance::edge_dash_flags(1.0, 999.0, 0.0, 0.0, 0.0) >> 12) & 0x3f, 63);
        assert_eq!((CellInstance::edge_orbit_flags(1.0, 1.0, 0.0, 0.0, 0.0) >> 24) & 0xf, 1, "주기 0 은 0.5초로");
    }
}
