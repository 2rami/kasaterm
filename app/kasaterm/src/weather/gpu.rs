//! 날씨 패스. 앱 프레임을 표면에 그린 뒤 그 표면을 한 장 복사해 입력으로 쓰고, 결과를
//! 표면에 다시 쓴다(Metal 표면은 샘플링할 수 없다). 날씨가 꺼져 있으면 이 모듈은 만들어지지도
//! 않는다 — 끈 사람의 비용은 0.
//!
//! 순서: 빗줄기(반해상도) → 프레임+빗줄기 → 판 파문(시뮬·굴절) → 창 유리(물 지도·흐림·합성)
//! → 단추 물방울과 함께 표면으로.

use bytemuck::{Pod, Zeroable};

const MAP_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SCENE_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub(crate) const MAX_INST: usize = 8192;
pub(crate) const MAX_PLACES: usize = 24;
pub(crate) const MAX_BUTTONS: usize = 64;
pub(crate) const MAX_ROWS: usize = 32;
pub(crate) const MAX_RIPPLES: usize = 64;

/// `KASATERM_WEATHER_TIMING=1`: log how long frames that draw only the weather take from
/// submit to GPU completion (p50/p95 every 60). Off by default — it waits on the GPU.
pub(crate) fn timing_requested() -> bool {
    std::env::var_os("KASATERM_WEATHER_TIMING").is_some()
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub(crate) struct Globals {
    pub res: [f32; 4],
    pub t: [f32; 4],
    pub rain: [f32; 4],
    pub rip: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct PlacesU {
    pub n: [f32; 4],
    pub rect: [[f32; 4]; MAX_PLACES],
    pub a: [[f32; 4]; MAX_PLACES],
    pub b: [[f32; 4]; MAX_PLACES],
    pub c: [[f32; 4]; MAX_PLACES],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct ButtonsU {
    pub n: [f32; 4],
    pub b: [[f32; 4]; MAX_BUTTONS],
    pub s: [[f32; 4]; MAX_BUTTONS],
    pub c: [[f32; 4]; MAX_BUTTONS],
    pub m: [[f32; 4]; MAX_BUTTONS],
    pub k: [[f32; 4]; MAX_ROWS],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RippleU {
    count: [f32; 4],
    d: [[f32; 4]; MAX_RIPPLES],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
pub(crate) struct Inst {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub extra: [f32; 4],
    pub clip: [f32; 4],
}

/// 한 프레임치 날씨 — CPU(`weather::App` 쪽)가 채우고 여기서 올려 그린다.
pub(crate) struct Frame {
    pub globals: Globals,
    pub places: PlacesU,
    pub buttons: ButtonsU,
    /// 단추 줄마다 칠할 상자(px).
    pub boxes: Vec<Inst>,
    /// 판 파문 떨어뜨리기: 텍셀 x, y, 반지름(텍셀), 세기.
    pub ripples: Vec<[f32; 4]>,
    pub ripple_steps: u32,
    pub ripple_dim: [u32; 2],
    /// 창 물 지도 인스턴스: 물방울 | 알갱이 | 와이퍼 | 김서림 (논리 px).
    pub inst: Vec<Inst>,
    pub counts: [u32; 4],
    /// 물 지도 캔버스(논리 px).
    pub canvas: [f32; 2],
    pub rain: bool,
    pub ripple: bool,
    pub glass: bool,
}

struct Tex {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
}

fn make_tex(device: &wgpu::Device, w: u32, h: u32, format: wgpu::TextureFormat, label: &str) -> Tex {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    Tex { tex, view }
}

fn shader(device: &wgpu::Device, src: &str, label: &str, regions: bool) -> wgpu::ShaderModule {
    let full = format!(
        "{}\n{}\n{}",
        include_str!("shaders/common.wgsl"),
        if regions { include_str!("shaders/regions.wgsl") } else { "" },
        src
    );
    device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(full.into()) })
}

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
    instanced: bool,
) -> wgpu::RenderPipeline {
    let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4];
    let inst_layout = [wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Inst>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &attrs,
    }];
    let strip = vs != "vs_full";
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(fs),
        layout: None,
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(vs),
            compilation_options: Default::default(),
            buffers: if instanced { &inst_layout } else { &[] },
        },
        primitive: wgpu::PrimitiveState {
            topology: if strip { wgpu::PrimitiveTopology::TriangleStrip } else { wgpu::PrimitiveTopology::TriangleList },
            ..Default::default()
        },
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn blend(src: wgpu::BlendFactor, dst: wgpu::BlendFactor) -> wgpu::BlendState {
    let c = wgpu::BlendComponent { src_factor: src, dst_factor: dst, operation: wgpu::BlendOperation::Add };
    wgpu::BlendState { color: c, alpha: c }
}

enum Res<'a> {
    Buf(&'a wgpu::Buffer),
    Samp(&'a wgpu::Sampler),
    View(&'a wgpu::TextureView),
}

fn bind(device: &wgpu::Device, p: &wgpu::RenderPipeline, entries: &[(u32, Res)]) -> wgpu::BindGroup {
    let e: Vec<wgpu::BindGroupEntry> = entries
        .iter()
        .map(|(n, r)| wgpu::BindGroupEntry {
            binding: *n,
            resource: match r {
                Res::Buf(b) => b.as_entire_binding(),
                Res::Samp(s) => wgpu::BindingResource::Sampler(s),
                Res::View(v) => wgpu::BindingResource::TextureView(v),
            },
        })
        .collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.get_bind_group_layout(0), entries: &e })
}

fn pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &'e wgpu::TextureView, clear: bool) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("weather"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if clear { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load },
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

fn full(enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup) {
    let mut rp = pass(enc, view, false);
    rp.set_pipeline(pipe);
    rp.set_bind_group(0, bg, &[]);
    rp.draw(0..3, 0..1);
}

fn draw(rp: &mut wgpu::RenderPass, inst: &wgpu::Buffer, pipe: &wgpu::RenderPipeline, bg: &wgpu::BindGroup, first: u32, count: u32) {
    if count > 0 {
        rp.set_pipeline(pipe);
        rp.set_bind_group(0, bg, &[]);
        rp.set_vertex_buffer(0, inst.slice(..));
        rp.draw(0..4, first..first + count);
    }
}

struct Pipes {
    rain: wgpu::RenderPipeline,
    scene: wgpu::RenderPipeline,
    copy: wgpu::RenderPipeline,
    panel: wgpu::RenderPipeline,
    ripple_drop: wgpu::RenderPipeline,
    ripple_update: wgpu::RenderPipeline,
    drop_map: wgpu::RenderPipeline,
    droplet: wgpu::RenderPipeline,
    erase: wgpu::RenderPipeline,
    erase_rect: wgpu::RenderPipeline,
    mist_rect: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    blur_h: wgpu::RenderPipeline,
    blur_v: wgpu::RenderPipeline,
    glass: wgpu::RenderPipeline,
    fin: wgpu::RenderPipeline,
    group: wgpu::RenderPipeline,
}

struct Buffers {
    globals: wgpu::Buffer,
    places: wgpu::Buffer,
    buttons: wgpu::Buffer,
    ripple: wgpu::Buffer,
    glass: wgpu::Buffer,
    inst: wgpu::Buffer,
    boxes: wgpu::Buffer,
}

struct Sized {
    w: u32,
    h: u32,
    ripple_dim: [u32; 2],
    frame: Tex,
    scene: [Tex; 2],
    rain: Tex,
    ripple: [Tex; 2],
    drops: Tex,
    droplets: Tex,
    mist: Tex,
    quarter: Tex,
    eighth: [Tex; 2],
    rain_bg: wgpu::BindGroup,
    scene_bg: wgpu::BindGroup,
    copy_bg: wgpu::BindGroup,
    panel: [[wgpu::BindGroup; 2]; 2],
    ripple_drop: [wgpu::BindGroup; 2],
    ripple_update: [wgpu::BindGroup; 2],
    drop_u: wgpu::BindGroup,
    droplet_u: wgpu::BindGroup,
    erase_u: wgpu::BindGroup,
    erase_rect_u: wgpu::BindGroup,
    mist_rect_u: wgpu::BindGroup,
    down: [wgpu::BindGroup; 3],
    blur_h: wgpu::BindGroup,
    blur_v: wgpu::BindGroup,
    glass: [wgpu::BindGroup; 2],
    fin: [wgpu::BindGroup; 2],
    group: [wgpu::BindGroup; 2],
}

pub(crate) struct WeatherGpu {
    format: wgpu::TextureFormat,
    pipes: Pipes,
    sampler: wgpu::Sampler,
    buf: Buffers,
    s: Option<Sized>,
    ripple_cur: usize,
    wall: Vec<f32>,
}

impl WeatherGpu {
    /// `format` is the surface format; the app frame is copied from and written back to it.
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let m_rain = shader(device, include_str!("shaders/rain.wgsl"), "weather rain", true);
        let m_panel = shader(device, include_str!("shaders/panel.wgsl"), "weather panel", true);
        let m_ripple = shader(device, include_str!("shaders/ripple.wgsl"), "weather ripple", true);
        let m_gmap = shader(device, include_str!("shaders/glass_map.wgsl"), "weather glass map", false);
        let m_glass = shader(device, include_str!("shaders/glass.wgsl"), "weather glass", true);
        let m_blur = shader(device, include_str!("shaders/blur.wgsl"), "weather blur", false);
        let m_btn = shader(device, include_str!("shaders/buttons.wgsl"), "weather buttons", false);
        use wgpu::BlendFactor as F;
        let over = Some(blend(F::One, F::OneMinusSrcAlpha));
        let wipe = Some(blend(F::Zero, F::OneMinusSrcAlpha));
        let p = |m, vs, fs, fmt, bl, inst| pipeline(device, m, vs, fs, fmt, bl, inst);
        let pipes = Pipes {
            rain: p(&m_rain, "vs_full", "fs_rain", MAP_FMT, None, false),
            scene: p(&m_rain, "vs_full", "fs_scene", SCENE_FMT, None, false),
            copy: p(&m_rain, "vs_full", "fs_copy", SCENE_FMT, None, false),
            panel: p(&m_panel, "vs_full", "fs_panel", SCENE_FMT, None, false),
            ripple_drop: p(&m_ripple, "vs_full", "fs_drop", MAP_FMT, None, false),
            ripple_update: p(&m_ripple, "vs_full", "fs_update", MAP_FMT, None, false),
            drop_map: p(&m_gmap, "vs_drop", "fs_drop", MAP_FMT, over, true),
            droplet: p(&m_gmap, "vs_drop", "fs_droplet", MAP_FMT, over, true),
            erase: p(&m_gmap, "vs_drop", "fs_erase", MAP_FMT, wipe, true),
            erase_rect: p(&m_gmap, "vs_rect", "fs_erase_rect", MAP_FMT, wipe, true),
            mist_rect: p(&m_gmap, "vs_rect", "fs_mist_rect", MAP_FMT, Some(blend(F::OneMinusDst, F::One)), true),
            down: p(&m_blur, "vs_full", "fs_down", SCENE_FMT, None, false),
            blur_h: p(&m_blur, "vs_full", "fs_blur_h", SCENE_FMT, None, false),
            blur_v: p(&m_blur, "vs_full", "fs_blur_v", SCENE_FMT, None, false),
            glass: p(&m_glass, "vs_full", "fs_glass", SCENE_FMT, None, false),
            fin: p(&m_btn, "vs_full", "fs_final", format, None, false),
            group: p(&m_btn, "vs_group", "fs_group", format, None, true),
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("weather"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let ubuf = |size: usize, label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let vbuf = |n: usize, label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (n * std::mem::size_of::<Inst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let buf = Buffers {
            globals: ubuf(std::mem::size_of::<Globals>(), "weather globals"),
            places: ubuf(std::mem::size_of::<PlacesU>(), "weather places"),
            buttons: ubuf(std::mem::size_of::<ButtonsU>(), "weather buttons"),
            ripple: ubuf(std::mem::size_of::<RippleU>(), "weather ripple"),
            glass: ubuf(16, "weather glass"),
            inst: vbuf(MAX_INST, "weather drops"),
            boxes: vbuf(MAX_ROWS, "weather button rows"),
        };
        WeatherGpu { format, pipes, sampler, buf, s: None, ripple_cur: 0, wall: Vec::new() }
    }

    pub(crate) fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, ripple_dim: [u32; 2]) {
        if self.s.as_ref().is_some_and(|s| s.w == w && s.h == h && s.ripple_dim == ripple_dim) {
            return;
        }
        let p = &self.pipes;
        let samp = &self.sampler;
        let buf = &self.buf;
        let frame = make_tex(device, w, h, self.format, "weather frame");
        let scene = [make_tex(device, w, h, SCENE_FMT, "weather scene0"), make_tex(device, w, h, SCENE_FMT, "weather scene1")];
        let rain = make_tex(device, w / 2, h / 2, MAP_FMT, "weather rain");
        let ripple = [
            make_tex(device, ripple_dim[0], ripple_dim[1], MAP_FMT, "weather ripple0"),
            make_tex(device, ripple_dim[0], ripple_dim[1], MAP_FMT, "weather ripple1"),
        ];
        let drops = make_tex(device, w / 2, h / 2, MAP_FMT, "weather drops");
        let droplets = make_tex(device, w / 2, h / 2, MAP_FMT, "weather droplets");
        let mist = make_tex(device, w / 4, h / 4, MAP_FMT, "weather mist");
        let quarter = make_tex(device, w / 4, h / 4, SCENE_FMT, "weather quarter");
        let eighth = [make_tex(device, w / 8, h / 8, SCENE_FMT, "weather eighth0"), make_tex(device, w / 8, h / 8, SCENE_FMT, "weather eighth1")];
        let mut enc = device.create_command_encoder(&Default::default());
        for t in [&ripple[0], &ripple[1], &drops, &droplets, &mist] {
            drop(pass(&mut enc, &t.view, true));
        }
        queue.submit([enc.finish()]);

        use Res::*;
        let g = || Buf(&buf.globals);
        let rg = || Buf(&buf.places);
        let rain_bg = bind(device, &p.rain, &[(0, g()), (9, rg())]);
        let scene_bg = bind(device, &p.scene, &[(0, g()), (1, Samp(samp)), (2, View(&frame.view)), (3, View(&rain.view))]);
        let copy_bg = bind(device, &p.copy, &[(1, Samp(samp)), (2, View(&frame.view))]);
        let panel = [0, 1].map(|si| {
            [0, 1].map(|ri| {
                bind(device, &p.panel, &[(0, g()), (1, Samp(samp)), (2, View(&scene[si].view)), (3, View(&ripple[ri].view)), (9, rg())])
            })
        });
        let ripple_drop =
            [0, 1].map(|i| bind(device, &p.ripple_drop, &[(0, g()), (1, Buf(&buf.ripple)), (2, View(&ripple[i].view)), (9, rg())]));
        let ripple_update = [0, 1].map(|i| bind(device, &p.ripple_update, &[(0, g()), (2, View(&ripple[i].view)), (9, rg())]));
        let gu = || Buf(&buf.glass);
        let drop_u = bind(device, &p.drop_map, &[(0, gu())]);
        let droplet_u = bind(device, &p.droplet, &[(0, gu())]);
        let erase_u = bind(device, &p.erase, &[(0, gu())]);
        let erase_rect_u = bind(device, &p.erase_rect, &[(0, gu())]);
        let mist_rect_u = bind(device, &p.mist_rect, &[(0, gu())]);
        let down = [&scene[0].view, &scene[1].view, &quarter.view].map(|v| bind(device, &p.down, &[(1, Samp(samp)), (2, View(v))]));
        let blur_h = bind(device, &p.blur_h, &[(1, Samp(samp)), (2, View(&eighth[0].view))]);
        let blur_v = bind(device, &p.blur_v, &[(1, Samp(samp)), (2, View(&eighth[1].view))]);
        let glass = [0, 1].map(|i| {
            bind(
                device,
                &p.glass,
                &[
                    (0, g()),
                    (1, Samp(samp)),
                    (2, View(&scene[i].view)),
                    (3, View(&eighth[0].view)),
                    (4, View(&drops.view)),
                    (5, View(&droplets.view)),
                    (6, View(&mist.view)),
                    (9, rg()),
                ],
            )
        });
        let fin = [0, 1].map(|i| bind(device, &p.fin, &[(1, Samp(samp)), (2, View(&scene[i].view))]));
        let group = [0, 1].map(|i| bind(device, &p.group, &[(0, g()), (1, Samp(samp)), (2, View(&scene[i].view)), (3, Buf(&buf.buttons))]));
        self.ripple_cur = 0;
        self.s = Some(Sized {
            w,
            h,
            ripple_dim,
            frame,
            scene,
            rain,
            ripple,
            drops,
            droplets,
            mist,
            quarter,
            eighth,
            rain_bg,
            scene_bg,
            copy_bg,
            panel,
            ripple_drop,
            ripple_update,
            drop_u,
            droplet_u,
            erase_u,
            erase_rect_u,
            mist_rect_u,
            down,
            blur_h,
            blur_v,
            glass,
            fin,
            group,
        });
    }

    /// Whether a frame has been copied since the last resize (a weather-only redraw needs it).
    pub(crate) fn has_frame(&self, w: u32, h: u32) -> bool {
        self.s.as_ref().is_some_and(|s| s.w == w && s.h == h)
    }

    /// Encodes the weather over `surface`. With `copy`, the app frame just drawn into
    /// `surface` becomes the input; without it the last copied frame is reused.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        surface: &wgpu::Texture,
        out: &wgpu::TextureView,
        f: &Frame,
        copy: bool,
    ) {
        let (w, h) = (surface.width(), surface.height());
        self.ensure(device, queue, w, h, f.ripple_dim);
        let Some(s) = self.s.as_ref() else { return };
        if copy {
            enc.copy_texture_to_texture(
                surface.as_image_copy(),
                s.frame.tex.as_image_copy(),
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        queue.write_buffer(&self.buf.globals, 0, bytemuck::bytes_of(&f.globals));
        queue.write_buffer(&self.buf.places, 0, bytemuck::bytes_of(&f.places));
        queue.write_buffer(&self.buf.buttons, 0, bytemuck::bytes_of(&f.buttons));
        queue.write_buffer(&self.buf.glass, 0, bytemuck::cast_slice(&[f.canvas[0], f.canvas[1], 1.0 / f.canvas[0], 1.0 / f.canvas[1]]));
        let n_inst = f.inst.len().min(MAX_INST);
        if n_inst > 0 {
            queue.write_buffer(&self.buf.inst, 0, bytemuck::cast_slice(&f.inst[..n_inst]));
        }
        let n_boxes = f.boxes.len().min(MAX_ROWS);
        if n_boxes > 0 {
            queue.write_buffer(&self.buf.boxes, 0, bytemuck::cast_slice(&f.boxes[..n_boxes]));
        }
        let p = &self.pipes;
        if f.rain {
            full(enc, &s.rain.view, &p.rain, &s.rain_bg);
        } else {
            full(enc, &s.scene[0].view, &p.copy, &s.copy_bg);
        }
        if f.rain {
            full(enc, &s.scene[0].view, &p.scene, &s.scene_bg);
        }
        let mut cur = 0usize;

        if f.ripple {
            let n = f.ripples.len().min(MAX_RIPPLES);
            if n > 0 {
                let mut u = RippleU::zeroed();
                u.count[0] = n as f32;
                u.d[..n].copy_from_slice(&f.ripples[..n]);
                queue.write_buffer(&self.buf.ripple, 0, bytemuck::bytes_of(&u));
                let src = self.ripple_cur;
                full(enc, &s.ripple[1 - src].view, &p.ripple_drop, &s.ripple_drop[src]);
                self.ripple_cur = 1 - src;
            }
            for _ in 0..f.ripple_steps {
                let src = self.ripple_cur;
                full(enc, &s.ripple[1 - src].view, &p.ripple_update, &s.ripple_update[src]);
                self.ripple_cur = 1 - src;
            }
            full(enc, &s.scene[1 - cur].view, &p.panel, &s.panel[cur][self.ripple_cur]);
            cur = 1 - cur;
        }

        if f.glass {
            let [n_drops, n_droplets, n_wipes, n_mist] = f.counts;
            let inst = &self.buf.inst;
            {
                let mut rp = pass(enc, &s.drops.view, true);
                draw(&mut rp, inst, &p.drop_map, &s.drop_u, 0, n_drops);
            }
            {
                let mut rp = pass(enc, &s.droplets.view, false);
                draw(&mut rp, inst, &p.droplet, &s.droplet_u, n_drops, n_droplets);
                draw(&mut rp, inst, &p.erase, &s.erase_u, 0, n_drops);
                draw(&mut rp, inst, &p.erase_rect, &s.erase_rect_u, n_drops + n_droplets, n_wipes);
            }
            {
                let mut rp = pass(enc, &s.mist.view, false);
                draw(&mut rp, inst, &p.mist_rect, &s.mist_rect_u, n_drops + n_droplets + n_wipes, n_mist);
                draw(&mut rp, inst, &p.erase, &s.erase_u, 0, n_drops);
                draw(&mut rp, inst, &p.erase_rect, &s.erase_rect_u, n_drops + n_droplets, n_wipes);
            }
            full(enc, &s.quarter.view, &p.down, &s.down[cur]);
            full(enc, &s.eighth[0].view, &p.down, &s.down[2]);
            full(enc, &s.eighth[1].view, &p.blur_h, &s.blur_h);
            full(enc, &s.eighth[0].view, &p.blur_v, &s.blur_v);
            full(enc, &s.scene[1 - cur].view, &p.glass, &s.glass[cur]);
            cur = 1 - cur;
        }

        let mut rp = pass(enc, out, false);
        rp.set_pipeline(&p.fin);
        rp.set_bind_group(0, &s.fin[cur], &[]);
        rp.draw(0..3, 0..1);
        draw(&mut rp, &self.buf.boxes, &p.group, &s.group[cur], 0, n_boxes as u32);
    }

    /// With timing on: one weather-only frame's submit→complete time.
    pub(crate) fn note_wall(&mut self, ms: f32) {
        self.wall.push(ms);
        if self.wall.len() >= 60 {
            let mut all = std::mem::take(&mut self.wall);
            all.sort_by(|a, b| a.total_cmp(b));
            eprintln!(
                "[weather-gpu] {} weather-only frames, submit→complete p50 {:.3}ms p95 {:.3}ms",
                all.len(),
                all[all.len() / 2],
                all[(all.len() * 95 / 100).min(all.len() - 1)]
            );
        }
    }
}
