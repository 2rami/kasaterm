
// 살아 움직이는 창 — Idle 모션을 돌리며 매 프레임 다시 그린다.
use mocari::render::common as rc;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct V { p: [f32; 2], uv: [f32; 2] }

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Xf { mvp: [f32; 16], mask_mtx: [f32; 16], channel: [f32; 4], opacity: f32, use_mask: f32, inverted: f32, _pad: f32 }

const MASK: u32 = 2048;

fn ortho(cw: f32, ch: f32) -> [f32; 16] {
    let mut m = [0.0f32; 16];
    m[0] = 2.0 / cw; m[5] = 2.0 / ch; m[10] = 1.0; m[15] = 1.0;
    m
}

struct Gfx {
    dev: wgpu::Device, q: wgpu::Queue, surf: wgpu::Surface<'static>, fmt: wgpu::TextureFormat,
    bgl: wgpu::BindGroupLayout, samp: wgpu::Sampler,
    texs: Vec<wgpu::TextureView>, mask_view: wgpu::TextureView, dummy_view: wgpu::TextureView,
    p_normal: wgpu::RenderPipeline, p_add: wgpu::RenderPipeline, p_mul: wgpu::RenderPipeline, p_mask: wgpu::RenderPipeline,
    p_plain: wgpu::RenderPipeline, bubble: Option<(wgpu::TextureView, f32, f32)>, bubble_vb: wgpu::Buffer, bubble_ub: wgpu::Buffer,
}

struct App {
    win: Option<Arc<Window>>, gfx: Option<Gfx>,
    x: f64, y: f64, w: f64, h: f64, scale: f32,
    alpha: wgpu::CompositeAlphaMode,
    cursor: (f64, f64), dragging: bool,
    bufs: Vec<Option<(wgpu::Buffer, wgpu::Buffer, u32)>>, ubs: Vec<wgpu::Buffer>,
    look: (f32, f32), look_now: (f32, f32),
    motion_params: std::collections::HashSet<String>,
    model: mocari::assets::RuntimeModel,
    motion: Option<mocari::motion::MotionPlayer>,
    last: std::time::Instant, t: f32, frames: u32, shots: u32, fps_t: std::time::Instant, fps_n: u32, dts: Vec<f32>,
    shot_path: Option<String>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() { return; }
        // 배너 창과 같은 규약: 다른 앱 위에 뜨되 **키 포커스를 안 뺏는다**.
        // 그 한 줄이 「타이핑 중에 끼어들지 않는다」의 전제다(notify_banner.rs).
        let win = Arc::new(el.create_window(
            Window::default_attributes()
                .with_title("곽향")
                .with_inner_size(winit::dpi::LogicalSize::new(self.w, self.h))
                .with_position(winit::dpi::LogicalPosition::new(self.x, self.y))
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
                .with_active(false)
        ).unwrap());
        // winit 의 with_transparent 만으로는 macOS 에서 창이 검은 판으로 남는다 —
        // NSWindow 를 직접 투명하게 세워야 뒤가 비친다.
        #[cfg(target_os = "macos")]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = win.window_handle() {
                if let RawWindowHandle::AppKit(h) = h.as_raw() {
                    unsafe {
                        let view: &objc2_app_kit::NSView = h.ns_view.cast().as_ref();
                        if let Some(nsw) = view.window() {
                            nsw.setOpaque(false);
                            nsw.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
                            nsw.setHasShadow(false);
                        }
                    }
                }
            }
        }
        let inst = wgpu::Instance::default();
        let surf = inst.create_surface(win.clone()).unwrap();
        let ad = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surf), ..Default::default() })).unwrap();
        let (dev, q) = pollster::block_on(ad.request_device(&Default::default())).unwrap();
        let caps = surf.get_capabilities(&ad);
        let fmt = caps.formats.iter().copied().find(|f| f.is_srgb()).unwrap_or(caps.formats[0]);
        let sz = win.inner_size();
        // 투명 창은 미리 곱한 알파로 합성해야 배경이 비친다 — 지원 안 하면 Auto 로 물러선다.
        eprintln!("alpha_modes: {:?} · formats: {:?}", caps.alpha_modes, &caps.formats[..caps.formats.len().min(4)]);
        // macOS Metal 은 PreMultiplied 를 안 내놓고 PostMultiplied 만 준다 — 그쪽을 고른다.
        // Opaque(기본 첫 항목)를 그대로 쓰면 창이 검은 판이 된다.
        let alpha = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied]
            .into_iter().find(|m| caps.alpha_modes.contains(m))
            .unwrap_or(caps.alpha_modes[0]);
        surf.configure(&dev, &wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format: fmt,
            width: sz.width.max(1), height: sz.height.max(1),
            present_mode: wgpu::PresentMode::Fifo, alpha_mode: alpha,
            view_formats: vec![], desired_maximum_frame_latency: 2 });
        self.alpha = alpha;

        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None, source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });
        let bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[
            wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
            wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            wgpu::BindGroupLayoutEntry { binding: 3, visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
        ]});
        let samp = dev.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge, address_mode_v: wgpu::AddressMode::ClampToEdge, ..Default::default() });
        let mut texs = Vec::new();
        for t in self.model.textures() {
            let size = wgpu::Extent3d { width: t.width(), height: t.height(), depth_or_array_layers: 1 };
            let tex = dev.create_texture(&wgpu::TextureDescriptor { label: None, size, mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
            q.write_texture(tex.as_image_copy(), t.rgba(), wgpu::TexelCopyBufferLayout {
                offset: 0, bytes_per_row: Some(4 * t.width()), rows_per_image: Some(t.height()) }, size);
            texs.push(tex.create_view(&Default::default()));
        }
        let mask_tex = dev.create_texture(&wgpu::TextureDescriptor { label: None,
            size: wgpu::Extent3d { width: MASK, height: MASK, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, view_formats: &[] });
        let mask_view = mask_tex.create_view(&Default::default());
        let dummy = dev.create_texture(&wgpu::TextureDescriptor { label: None,
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
        q.write_texture(dummy.as_image_copy(), &[0u8; 4], wgpu::TexelCopyBufferLayout {
            offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 });
        let dummy_view = dummy.create_view(&Default::default());

        let pl = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], ..Default::default() });
        let mk = |entry: &str, f: wgpu::TextureFormat, blend: wgpu::BlendState| dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None, layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: 16, step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2] }] },
            fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some(entry), compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: f, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })] }),
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(), cache: None, multiview_mask: None });
        let over = wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add } };
        let add = wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add } };
        let mul = wgpu::BlendState { color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Dst, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
            alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add } };
        // 말풍선 — 미리 그린 PNG 를 텍스처로. 대사는 character.json 이 준다.
        let bubble = std::env::var("BUBBLE").ok().and_then(|p| {
            let f = std::fs::File::open(&p).ok()?;
            let mut r = png::Decoder::new(std::io::BufReader::new(f)).read_info().ok()?;
            let mut buf = vec![0u8; r.output_buffer_size()];
            let info = r.next_frame(&mut buf).ok()?;
            let (bw, bh) = (info.width, info.height);
            let size = wgpu::Extent3d { width: bw, height: bh, depth_or_array_layers: 1 };
            let tex = dev.create_texture(&wgpu::TextureDescriptor { label: None, size, mip_level_count: 1, sample_count: 1,
                dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[] });
            q.write_texture(tex.as_image_copy(), &buf[..(bw * bh * 4) as usize],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * bw), rows_per_image: Some(bh) }, size);
            Some((tex.create_view(&Default::default()), bw as f32, bh as f32))
        });
        let bubble_vb = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 6 * 16,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bubble_ub = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 160,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        self.gfx = Some(Gfx {
            p_normal: mk("fs", fmt, over), p_add: mk("fs", fmt, add), p_mul: mk("fs", fmt, mul),
            p_mask: mk("fs_mask", wgpu::TextureFormat::Rgba8Unorm, add), p_plain: mk("fs_plain", fmt, over),
            bubble, bubble_vb, bubble_ub,
            dev, q, surf, fmt, bgl, samp, texs, mask_view, dummy_view });
        self.win = Some(win);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(sz) => {
                if let Some(g) = &self.gfx {
                    let caps_alpha = self.alpha;
                    g.surf.configure(&g.dev, &wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format: g.fmt,
                        width: sz.width.max(1), height: sz.height.max(1),
                        present_mode: wgpu::PresentMode::Fifo, alpha_mode: caps_alpha,
                        view_formats: vec![], desired_maximum_frame_latency: 2 });
                }
            }
            // 아무 데나 잡아 끌면 창이 따라온다 — 장식이 없어 제목 표시줄이 없다.
            // OS 에 끌기를 넘기면(drag_window) 끄는 동안의 좌표 계산·모니터 경계를
            // 우리가 안 만져도 된다.
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if let Some(w) = &self.win { let _ = w.drag_window(); }
            }
            WindowEvent::CursorMoved { position, .. } => { self.cursor = (position.x, position.y); }
            // 휠로 크기 — 캐릭터만 커지고 창도 함께 자란다. 0.4~3배로 묶어 화면 밖으로
            // 나가거나 점이 되는 것을 막는다.
            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                let old = self.scale;
                self.scale = (self.scale * (1.0 + d * 0.06)).clamp(0.4, 3.0);
                if (self.scale - old).abs() > 0.001 {
                    if let Some(w) = &self.win {
                        let _ = w.request_inner_size(winit::dpi::LogicalSize::new(
                            self.w * self.scale as f64, self.h * self.scale as f64));
                    }
                }
            }
            // 오른쪽 단추 = 끝내기(장식이 없어 닫기 단추가 없다).
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => el.exit(),
            WindowEvent::RedrawRequested => self.draw(el),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(w) = &self.win { w.request_redraw(); }
    }
}

impl App {
    /// 화면 전체 기준 커서 — 창 밖에 있어도 안다. 창 중심을 원점으로 -1..1 로 준다.
    #[cfg(target_os = "macos")]
    fn poll_cursor(&mut self) {
        let Some(w) = &self.win else { return };
        let Ok(pos) = w.outer_position() else { return };
        let sz = w.inner_size();
        let sf = w.scale_factor();
        let p = unsafe { objc2_app_kit::NSEvent::mouseLocation() };
        // AppKit 은 왼쪽 **아래**가 원점이라 y 를 뒤집어야 winit 좌표와 만난다.
        let screen_h = objc2_app_kit::NSScreen::mainScreen(objc2_foundation::MainThreadMarker::new().unwrap())
            .map(|s| s.frame().size.height).unwrap_or(1080.0);
        let (gx, gy) = (p.x * sf, (screen_h - p.y) * sf);
        let (cx, cy) = (pos.x as f64 + sz.width as f64 / 2.0, pos.y as f64 + sz.height as f64 / 2.0);
        // 창 두 배 거리에서 최대로 돌아본다 — 더 멀면 고개가 끝까지 돌아간 채 멈춘다.
        let nx = ((gx - cx) / (sz.width as f64)).clamp(-1.0, 1.0);
        let ny = ((cy - gy) / (sz.height as f64)).clamp(-1.0, 1.0);
        self.look = (nx as f32, ny as f32);
    }
    #[cfg(not(target_os = "macos"))]
    fn poll_cursor(&mut self) {}

    fn draw(&mut self, el: &ActiveEventLoop) {
        self.poll_cursor();
        let dt = self.last.elapsed().as_secs_f32().min(0.1);
        self.last = std::time::Instant::now();

        // 모션 → 파라미터 → 메시
        {
            let rt = self.model.runtime_mut();
            rt.reset_parameters();
            if let Some(m) = &mut self.motion { m.tick(dt); m.apply(rt); }
            // Cubism 런타임이 자동으로 하는 것 — 모션 파일에는 없다.
            self.t += dt;
            let t = self.t;
            // 모션이 쓰는 파라미터는 건드리지 않는다. 덮어쓰면 연출과 싸워서
            // 고개가 튀고 눈이 깜빡이다 만다 — 모션 하나가 8개를 쥐고 있다(실측).
            // Cubism 도 같은 규약이다: 자동 효과는 모션이 **안 쓰는** 것에만 얹는다.
            let owned = &self.motion_params;
            let mut put = |rt: &mut mocari::runtime::ModelRuntime, id: &str, v: f32| {
                if !owned.contains(id) { rt.set_parameter(id, v); }
            };
            put(rt, "ParamBreath", (t * 1.6).sin() * 0.5 + 0.5);
            // 마우스를 쳐다본다. 창이 포커스를 안 받으므로(with_active(false)) 창 안
            // 이벤트로는 커서를 못 본다 — OS 에 전역 위치를 직접 묻는다.
            let (mx, my) = self.look;
            let ease = 1.0 - (-dt * 6.0).exp();
            self.look_now.0 += (mx - self.look_now.0) * ease;
            self.look_now.1 += (my - self.look_now.1) * ease;
            let (lx, ly) = self.look_now;
            put(rt, "ParamAngleX", lx * 30.0);
            put(rt, "ParamAngleY", ly * 30.0);
            put(rt, "ParamAngleZ", lx * ly * -10.0);
            put(rt, "ParamEyeBallX", lx);
            put(rt, "ParamEyeBallY", ly);
            put(rt, "ParamBodyAngleX", lx * 10.0);
            put(rt, "ParamBodyAngleY", ly * 5.0);
            let blink = { let c = t % 4.0; if c < 0.06 { 1.0 - c / 0.06 } else if c < 0.12 { (c - 0.06) / 0.06 } else { 1.0 } };
            put(rt, "ParamEyeLOpen", blink);
            put(rt, "ParamEyeROpen", blink);
            rt.apply_physics(dt);
            rt.update_meshes();
        }
        let Some(g) = &self.gfx else { return };
        let rt = self.model.runtime();
        let canvas = rt.canvas();
        let ppu = canvas.pixels_per_unit();
        let (cw, ch) = (canvas.width() / ppu, canvas.height() / ppu);
        let meshes = rt.meshes();
        let infos: Vec<rc::DrawableInfo> = meshes.iter().map(rc::DrawableInfo::from_mesh).collect();
        let mut plan = rc::ClippingPlan::from_drawables(&infos);
        let _ = plan.prepare_single_texture_masks(&infos);

        // 정점 수는 모션이 바뀌어도 그대로다 — 버퍼는 **한 번만** 만들고 값만 갱신한다.
        // 매 프레임 새로 만들면 드로어블 326개 × 60fps 로 쌓여 RSS 가 1GB 를 넘긴다(실측).
        if self.bufs.is_empty() {
            for m in meshes.iter() {
                let vs = rc::vertices_from_drawable(m);
                let idx = m.indices();
                if vs.is_empty() || idx.is_empty() { self.bufs.push(None); continue; }
                let verts: Vec<V> = vs.iter().map(|v| { let p = v.position(); let uv = v.uv(); V { p: [p[0], p[1]], uv: [uv[0], uv[1]] } }).collect();
                self.bufs.push(Some((
                    g.dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: (verts.len() * 16) as u64,
                        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }),
                    g.dev.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(idx), usage: wgpu::BufferUsages::INDEX }),
                    idx.len() as u32)));
                if let Some(Some((vb, _, _))) = self.bufs.last() { g.q.write_buffer(vb, 0, bytemuck::cast_slice(&verts)); }
            }
            // 유니폼도 드로어블마다 하나씩 — 마스크 패스와 본 패스가 각각 쓴다.
            for _ in 0..meshes.len() * 2 {
                self.ubs.push(g.dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 160,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }));
            }
        } else {
            for (i, m) in meshes.iter().enumerate() {
                let Some(Some((vb, _, _))) = self.bufs.get(i) else { continue };
                let vs = rc::vertices_from_drawable(m);
                if vs.is_empty() { continue; }
                let verts: Vec<V> = vs.iter().map(|v| { let p = v.position(); let uv = v.uv(); V { p: [p[0], p[1]], uv: [uv[0], uv[1]] } }).collect();
                g.q.write_buffer(vb, 0, bytemuck::cast_slice(&verts));
            }
        }
        let bufs = &self.bufs;
        let base = ortho(cw, ch);
        let frame = match g.surf.get_current_texture() { Ok(f) => f, Err(_) => return };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = g.dev.create_command_encoder(&Default::default());
        let bind = |ub: &wgpu::Buffer, tex: &wgpu::TextureView, mask: &wgpu::TextureView| g.dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &g.bgl, entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(tex) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&g.samp) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(mask) },
            ]});
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("mask"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &g.mask_view, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            rp.set_pipeline(&g.p_mask);
            for ctx in plan.contexts() {
                let (Some(layout), Some(mtx)) = (ctx.layout(), ctx.matrix_for_mask()) else { continue };
                for &mi in ctx.masks() {
                    let mi = mi as usize;
                    let Some((vb, ib, n)) = bufs.get(mi).and_then(|b| b.as_ref()) else { continue };
                    let u = Xf { mvp: *mtx.as_slice(), mask_mtx: base, channel: layout.channel_flag(), opacity: 1.0, use_mask: 0.0, inverted: 0.0, _pad: 0.0 };
                    let ub = &self.ubs[mi];
                    g.q.write_buffer(ub, 0, bytemuck::bytes_of(&u));
                    let bg = bind(ub, &g.texs[(infos[mi].texture_index() as usize).min(g.texs.len()-1)], &g.dummy_view);
                    rp.set_bind_group(0, &bg, &[]);
                    rp.set_vertex_buffer(0, vb.slice(..));
                    rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                    rp.draw_indexed(0..*n, 0, 0..1);
                }
            }
        }
        let order = rc::draw_order_indices(&infos);
        let mut ctx_of: std::collections::HashMap<usize, &rc::ClippingContext> = Default::default();
        for c in plan.contexts() { for &d in c.drawable_indices() { ctx_of.insert(d, c); } }
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor { label: Some("model"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, resolve_target: None, depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
            for &i in &order {
                let info = &infos[i];
                if !info.is_visible() || info.opacity() <= 0.001 { continue; }
                let Some((vb, ib, n)) = bufs.get(i).and_then(|b| b.as_ref()) else { continue };
                let (mask_mtx, use_mask, chan, inv) = match ctx_of.get(&i) {
                    Some(c) => match (c.matrix_for_draw(), c.layout()) {
                        (Some(m), Some(l)) => (*m.as_slice(), 1.0, l.channel_flag(), if c.inverted() { 1.0 } else { 0.0 }),
                        _ => (base, 0.0, [0.0; 4], 0.0) },
                    None => (base, 0.0, [0.0; 4], 0.0) };
                let u = Xf { mvp: base, mask_mtx, channel: chan, opacity: info.opacity(), use_mask, inverted: inv, _pad: 0.0 };
                let ub = &self.ubs[meshes.len() + i];
                g.q.write_buffer(ub, 0, bytemuck::bytes_of(&u));
                let bg = bind(ub, &g.texs[(info.texture_index() as usize).min(g.texs.len()-1)], &g.mask_view);
                rp.set_pipeline(match format!("{:?}", info.blend_mode()).as_str() {
                    s if s.contains("Additive") => &g.p_add,
                    s if s.contains("Multipl") => &g.p_mul,
                    _ => &g.p_normal });
                rp.set_bind_group(0, &bg, &[]);
                rp.set_vertex_buffer(0, vb.slice(..));
                rp.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint16);
                rp.draw_indexed(0..*n, 0, 0..1);
            }
            // 말풍선 — 캐릭터 머리 위 오른쪽(character.json 의 offset 규약).
            if let Some((bv, bw, bh)) = &g.bubble {
                let win = self.win.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                let (sw, sh) = (win.width.max(1) as f32, win.height.max(1) as f32);
                // 화면 폭의 4/5 를 넘지 않게 — 넘치면 캐릭터를 가린다.
                let scale = (sw * 0.8 / bw).min(1.5);
                let (w2, h2) = (bw * scale / sw, bh * scale / sh);
                let (x0, y0) = (-0.55, 0.95);
                let quad = [
                    V { p: [x0, y0], uv: [0.0, 0.0] },
                    V { p: [x0 + w2 * 2.0, y0], uv: [1.0, 0.0] },
                    V { p: [x0, y0 - h2 * 2.0], uv: [0.0, 1.0] },
                    V { p: [x0 + w2 * 2.0, y0], uv: [1.0, 0.0] },
                    V { p: [x0 + w2 * 2.0, y0 - h2 * 2.0], uv: [1.0, 1.0] },
                    V { p: [x0, y0 - h2 * 2.0], uv: [0.0, 1.0] },
                ];
                g.q.write_buffer(&g.bubble_vb, 0, bytemuck::cast_slice(&quad));
                let mut m = [0.0f32; 16]; m[0] = 1.0; m[5] = 1.0; m[10] = 1.0; m[15] = 1.0;
                let u = Xf { mvp: m, mask_mtx: m, channel: [0.0; 4], opacity: 1.0, use_mask: 0.0, inverted: 0.0, _pad: 0.0 };
                g.q.write_buffer(&g.bubble_ub, 0, bytemuck::bytes_of(&u));
                let bg = bind(&g.bubble_ub, bv, &g.dummy_view);
                rp.set_pipeline(&g.p_plain);
                rp.set_bind_group(0, &bg, &[]);
                rp.set_vertex_buffer(0, g.bubble_vb.slice(..));
                rp.draw(0..6, 0..1);
            }
        }
        g.q.submit([enc.finish()]);
        frame.present();
        self.frames += 1;
        self.fps_n += 1;
        self.dts.push(dt * 1000.0);
        if self.fps_t.elapsed().as_secs_f32() >= 1.0 {
            let f = self.fps_n as f32 / self.fps_t.elapsed().as_secs_f32();
            let mut v = self.dts.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (min, med, p95, max) = (v[0], v[v.len()/2], v[v.len()*95/100], v[v.len()-1]);
            let mean: f32 = v.iter().sum::<f32>() / v.len() as f32;
            let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
            eprintln!("FPS {f:.1} · 프레임시간 최소 {min:.1} 중앙 {med:.1} p95 {p95:.1} 최대 {max:.1} 흔들림 {sd:.1}ms");
            self.dts.clear();
            self.fps_n = 0; self.fps_t = std::time::Instant::now();
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let model = mocari::assets::load_model_runtime(&path).expect("모델");
    let dir = std::path::Path::new(&path).parent().unwrap().to_path_buf();
    let file = std::env::args().nth(2).unwrap_or_else(|| "zhentou.motion3.json".to_string());
    // 그 모션이 쥐고 있는 파라미터 — 자동 효과가 이것들을 피해 간다.
    let motion_params: std::collections::HashSet<String> = std::fs::read_to_string(dir.join(&file)).ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("Curves").and_then(|c| c.as_array()).map(|a| a.iter()
            .filter(|c| c.get("Target").and_then(|t| t.as_str()) == Some("Parameter"))
            .filter_map(|c| c.get("Id").and_then(|i| i.as_str()).map(str::to_string)).collect()))
        .unwrap_or_default();
    eprintln!("모션이 쥔 파라미터 {}개", motion_params.len());
    let motion = mocari::motion::load_motion(dir.join(&file)).ok()
        .map(mocari::motion::MotionPlayer::new);
    eprintln!("모션 파일: {file}");
    eprintln!("모션 로드: {}", if motion.is_some() { "성공" } else { "실패" });
    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App { win: None, gfx: None,
        x: 60.0, y: 80.0, w: 420.0, h: 600.0, scale: 1.0,
        alpha: wgpu::CompositeAlphaMode::Auto, cursor: (0.0, 0.0), dragging: false, bufs: Vec::new(), ubs: Vec::new(), look: (0.0, 0.0), look_now: (0.0, 0.0), motion_params,
        model, motion, last: std::time::Instant::now(), t: 0.0, fps_t: std::time::Instant::now(), fps_n: 0, dts: Vec::new(), frames: 0, shots: 0, shot_path: None };
    el.run_app(&mut app).unwrap();
}
