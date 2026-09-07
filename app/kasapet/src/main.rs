
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

mod board;
mod bubble;

/// 말풍선이 앉을 자리 — 창 위쪽을 이만큼 비우고 캐릭터를 그 아래에 그린다.
/// 안 비우면 캐릭터가 창을 꽉 채워 말풍선이 화면 끝에 눌린 채 못 움직인다.
const HEADROOM: f64 = 110.0;


struct Gfx {
    dev: wgpu::Device, q: wgpu::Queue, surf: wgpu::Surface<'static>, fmt: wgpu::TextureFormat,
    bgl: wgpu::BindGroupLayout, samp: wgpu::Sampler,
    texs: Vec<wgpu::TextureView>, mask_view: wgpu::TextureView, dummy_view: wgpu::TextureView,
    p_normal: wgpu::RenderPipeline, p_add: wgpu::RenderPipeline, p_mul: wgpu::RenderPipeline, p_mask: wgpu::RenderPipeline,
    p_plain: wgpu::RenderPipeline,
    bubble_vb: wgpu::Buffer, bubble_ub: wgpu::Buffer,
}

struct App {
    win: Option<Arc<Window>>, gfx: Option<Gfx>,
    x: f64, y: f64, w: f64, h: f64, scale: f32,
    alpha: wgpu::CompositeAlphaMode,
    cursor: (f64, f64),
    /// 캐릭터 폴더들이 모인 자리(`~/.config/kasaterm/pet`). 자리·크기 저장과 캐릭터
    /// 바꾸기가 여기를 본다. 모델을 env 로 직접 준 검증 실행에서는 없다.
    pet_dir: Option<std::path::PathBuf>,
    name: String,
    /// 두 번 누름 판정용. 왼쪽 한 번은 끌기라, 바로 끌어 버리면 두 번째를 못 본다.
    last_click: Option<std::time::Instant>,
    /// 지금 판. kasaterm 이 적어 두는 파일에서 온다.
    mood: board::Mood,
    say: String,
    board_seen: Option<std::time::SystemTime>,
    board_polled: std::time::Instant,
    /// 마지막으로 뭔가 벌어진 때. 오래 조용하면 잠든다.
    stirred: std::time::Instant,
    /// 글자를 그린 텍스처. 말풍선은 이것이 있을 때만 뜬다.
    bubble_text: Option<(wgpu::TextureView, f32, f32)>,
    /// 모델의 맨 윗점(모델 좌표). 말풍선이 이 점을 따라다녀 몸이 흔들리면 함께 흔들린다.
    head: (f32, f32),
    /// 그림이 실제로 차지하는 범위. 모델이 선언한 캔버스보다 큰 경우가 흔해(마오는 모자가
    /// 30% 삐져나온다) 캔버스에 맞춰 그리면 잘리고, 머리 위 자리 계산도 어긋난다.
    bbox: Option<(f32, f32, f32, f32)>,
    /// 무리별 모션 파일. 상태가 바뀌면 여기서 하나 고른다.
    motion_files: Vec<std::path::PathBuf>,
    expr_files: Vec<std::path::PathBuf>,
    exprs: mocari::expression::ExpressionManager,
    bufs: Vec<Option<(wgpu::Buffer, wgpu::Buffer, u32)>>, ubs: Vec<wgpu::Buffer>,
    look: (f32, f32), look_now: (f32, f32),
    motion_params: std::collections::HashSet<String>,
    model: mocari::assets::RuntimeModel,
    motion: Option<mocari::motion::MotionPlayer>,
    last: std::time::Instant, t: f32, frames: u32, fps_t: std::time::Instant, fps_n: u32, dts: Vec<f32>,
    shot_path: Option<String>,
    /// 이 프레임에 이르면 화면을 파일로 뜬다. 사람 눈 대신 쓰는 검증 창구다.
    shot_at: u32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.win.is_some() { return; }
        // 배너 창과 같은 규약: 다른 앱 위에 뜨되 **키 포커스를 안 뺏는다**.
        // 그 한 줄이 「타이핑 중에 끼어들지 않는다」의 전제다(notify_banner.rs).
        let win = Arc::new(el.create_window(
            Window::default_attributes()
                .with_title(&self.name)
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, format: fmt,
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
        let bubble_vb = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 6 * 16,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bubble_ub = dev.create_buffer(&wgpu::BufferDescriptor { label: None, size: 160,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        self.gfx = Some(Gfx {
            p_normal: mk("fs", fmt, over), p_add: mk("fs", fmt, add), p_mul: mk("fs", fmt, mul),
            p_mask: mk("fs_mask", wgpu::TextureFormat::Rgba8Unorm, add), p_plain: mk("fs_plain", fmt, over),
            bubble_vb, bubble_ub,
            dev, q, surf, fmt, bgl, samp, texs, mask_view, dummy_view });
        self.win = Some(win);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            // 배율이 다른 모니터로 끌고 가면 이것이 먼저 온다. 안 받으면 창이 옛 배율의
            // 물리 크기를 그대로 쥐고 있어 캐릭터가 반쪽이 되거나 흐릿해진다 — 논리 크기를
            // 다시 걸어 주면 뒤따르는 Resized 가 맞는 물리 크기를 들고 온다.
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = &self.win {
                    let _ = w.request_inner_size(winit::dpi::LogicalSize::new(
                        self.w * self.scale as f64,
                        self.h * self.scale as f64,
                    ));
                }
            }
            WindowEvent::Resized(sz) => {
                if let Some(g) = &self.gfx {
                    let caps_alpha = self.alpha;
                    g.surf.configure(&g.dev, &wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, format: g.fmt,
                        width: sz.width.max(1), height: sz.height.max(1),
                        present_mode: wgpu::PresentMode::Fifo, alpha_mode: caps_alpha,
                        view_formats: vec![], desired_maximum_frame_latency: 2 });
                }
            }
            // 아무 데나 잡아 끌면 창이 따라온다 — 장식이 없어 제목 표시줄이 없다.
            // OS 에 끌기를 넘기면(drag_window) 끄는 동안의 좌표 계산·모니터 경계를
            // 우리가 안 만져도 된다.
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let double = self
                    .last_click
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(400));
                self.last_click = Some(std::time::Instant::now());
                if double {
                    self.next_character();
                    return;
                }
                self.touch();
                if let Some(w) = &self.win { let _ = w.drag_window(); }
            }
            WindowEvent::CursorMoved { position, .. } => { self.cursor = (position.x, position.y); }
            // 끌어 옮긴 자리는 그 자리에서 적어 둔다 — 종료를 기다리면 SIGTERM(하단바
            // 끄기)으로 죽을 때 못 남긴다.
            WindowEvent::Moved(pos) => {
                let f = self.win.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                self.x = pos.x as f64 / f;
                self.y = pos.y as f64 / f;
                self.save_state();
            }
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
                    self.save_state();
                }
            }
            // 오른쪽 단추 = 끝내기(장식이 없어 닫기 단추가 없다).
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => el.exit(),
            WindowEvent::RedrawRequested => self.draw(),
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
        let sf = w.scale_factor();
        // 전부 논리 좌표(pt)로 계산한다. AppKit 의 마우스 좌표가 pt 이고, 물리 픽셀로
        // 맞추려면 배율을 곱해야 하는데 그 배율은 **창이 있는 화면**의 것이라 배율이 다른
        // 모니터가 섞이면 어긋난다(맥북 2배 + 외장 4K 1배).
        let pos = pos.to_logical::<f64>(sf);
        let sz = w.inner_size().to_logical::<f64>(sf);
        let p = objc2_app_kit::NSEvent::mouseLocation();
        // AppKit 은 왼쪽 **아래**가 원점이라 y 를 뒤집어야 winit 좌표와 만난다. 기준은
        // **주 화면**(메뉴바가 있는 화면)의 높이다 — mainScreen 은 「지금 활성인 화면」이라
        // 외장 모니터에서는 다른 값이 나와 시선이 엉뚱한 데를 본다.
        let mtm = objc2_foundation::MainThreadMarker::new().unwrap();
        let screen_h = objc2_app_kit::NSScreen::screens(mtm)
            .iter()
            .next()
            .map(|s| s.frame().size.height)
            .unwrap_or(1080.0);
        let (gx, gy) = (p.x, screen_h - p.y);
        let (cx, cy) = (pos.x + sz.width / 2.0, pos.y + sz.height / 2.0);
        // 창 두 배 거리에서 최대로 돌아본다 — 더 멀면 고개가 끝까지 돌아간 채 멈춘다.
        let nx = ((gx - cx) / sz.width.max(1.0)).clamp(-1.0, 1.0);
        let ny = ((cy - gy) / sz.height.max(1.0)).clamp(-1.0, 1.0);
        self.look = (nx as f32, ny as f32);
    }

    #[cfg(not(target_os = "macos"))]
    fn poll_cursor(&mut self) {}

    /// 자리와 크기를 남긴다. 펫은 켤 때마다 같은 데서 뜨는 편이 자연스럽고,
    /// 매번 왼쪽 위로 돌아가면 옮긴 일이 헛일이 된다.
    fn save_state(&self) {
        let Some(d) = &self.pet_dir else { return };
        let j = format!(
            "{{\"x\":{:.0},\"y\":{:.0},\"scale\":{:.3}}}",
            self.x, self.y, self.scale
        );
        let _ = std::fs::write(d.join("state.json"), j);
    }

    /// 다음 캐릭터로. 프로세스를 바꿔치기(`execv`)하는 이유는 **pid 를 지키기 위해서**다 —
    /// 하단바 칩은 pid 파일로 켜짐을 판정하므로, 새로 spawn 하면 그 칩이 꺼진 것으로 읽힌다.
    fn next_character(&mut self) {
        let Some(d) = self.pet_dir.clone() else { return };
        let mut names: Vec<String> = std::fs::read_dir(&d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| model3_in(p).is_some())
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        if names.len() < 2 {
            return;
        }
        let i = names.iter().position(|n| *n == self.name).unwrap_or(0);
        let next = &names[(i + 1) % names.len()];
        let Some(model) = model3_in(&d.join(next)) else { return };
        let _ = std::fs::write(d.join("current"), next);
        self.save_state();
        exec_self(&model);
    }

    /// kasaterm 이 적어 둔 판을 읽는다. 파일이 안 바뀌었으면 아무 일도 안 한다 —
    /// 앱이 꺼져 있으면 영영 안 바뀌고, 그건 「조용하다」로 읽으면 그만이다.
    fn poll_board(&mut self) {
        if self.board_polled.elapsed() < std::time::Duration::from_millis(700) {
            return;
        }
        self.board_polled = std::time::Instant::now();
        let Some(d) = self.pet_dir.clone() else { return };
        let f = d.join("board.json");
        let m = std::fs::metadata(&f).and_then(|m| m.modified()).ok();
        if m == self.board_seen {
            // 오래 조용하면 잠든다. 판이 안 바뀌는 동안에만 세므로, 뭔가 벌어지면
            // 아래에서 stirred 가 갱신돼 잠이 풀린다.
            if self.mood == board::Mood::Idle && self.stirred.elapsed() > board::SLEEP_AFTER {
                self.apply_mood(board::Mood::Sleep);
            }
            return;
        }
        self.board_seen = m;
        let (mood, text) = board::read(&f);
        self.stirred = std::time::Instant::now();
        if text != self.say {
            self.say = text;
            self.rebuild_bubble_text();
        }
        if mood != self.mood {
            self.apply_mood(mood);
        }
    }

    /// 상태가 바뀌면 몸도 바뀐다 — 모션 하나와 표정 하나.
    fn apply_mood(&mut self, mood: board::Mood) {
        self.mood = mood;
        // 공식 샘플의 모션 무리는 `Idle`·`TapBody` 뿐이라 이름으로는 못 고른다.
        // 상태마다 자리를 하나씩 주고, 모델이 가진 수로 나눠 쓴다.
        if !self.motion_files.is_empty() {
            let i = mood.slot() % self.motion_files.len();
            self.play_motion(i, true);
        }
        // 표정은 이름이 없는 모델이 많아(exp_01…) 뜻으로 못 고른다. 있는 만큼만 갈라 쓰고,
        // 없는 모델은 표정 없이 모션으로만 상태를 보인다.
        self.exprs.stop_all();
        if !self.expr_files.is_empty() && mood != board::Mood::Idle {
            let f = &self.expr_files[mood.slot() % self.expr_files.len()];
            if let Ok(e) = mocari::expression::load_expression(f) {
                self.exprs.play(e);
            }
        }
    }

    /// 쓰다듬으면 반응한다 — 마지막 모션 자리를 이 몫으로 둔다. `TapBody` 무리가
    /// 목록 뒤쪽에 오므로 대개 그 안에서 걸린다.
    fn touch(&mut self) {
        self.stirred = std::time::Instant::now();
        if self.motion_files.is_empty() {
            return;
        }
        // 마지막 자리를 쓰다듬기 몫으로 둔다 — `TapBody` 무리가 목록 뒤쪽에 오므로
        // 대개 그 안에서 걸린다. 한 번만 돌고 끝나면 원래 상태로 돌아간다.
        let i = self.motion_files.len() - 1;
        self.play_motion(i, false);
    }

    /// 모션을 갈아 끼운다. 파일과 「그 모션이 쥔 파라미터」는 늘 짝이어야 한다 —
    /// 어긋나면 자동 효과(숨·눈·시선)가 모션과 싸워 고개가 튀고 눈이 깜빡이다 만다.
    fn play_motion(&mut self, i: usize, looping: bool) {
        let Some(f) = self.motion_files.get(i).cloned() else { return };
        self.motion_params = std::fs::read_to_string(&f)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| {
                v.get("Curves").and_then(|c| c.as_array()).map(|a| {
                    a.iter()
                        .filter(|c| c.get("Target").and_then(|t| t.as_str()) == Some("Parameter"))
                        .filter_map(|c| c.get("Id").and_then(|i| i.as_str()).map(str::to_string))
                        .collect()
                })
            })
            .unwrap_or_default();
        self.motion = mocari::motion::load_motion(&f)
            .ok()
            .map(|m| mocari::motion::MotionPlayer::with_looping(m, looping));
    }

    /// 할 말을 글자 텍스처로. 빈 말이면 말풍선이 통째로 사라진다.
    fn rebuild_bubble_text(&mut self) {
        let Some(g) = &self.gfx else { return };
        self.bubble_text = bubble::render_text(&g.dev, &g.q, &self.say, 260.0);
    }

    fn draw(&mut self) {
        self.poll_cursor();
        self.poll_board();
        // 쓰다듬기처럼 한 번만 도는 모션이 끝났으면 지금 상태로 돌아간다.
        if self.motion.as_ref().is_some_and(|m| !m.is_looping() && m.is_finished()) {
            self.apply_mood(self.mood);
        }
        let dt = self.last.elapsed().as_secs_f32().min(0.1);
        self.last = std::time::Instant::now();

        // 모션 → 파라미터 → 메시
        {
            let rt = self.model.runtime_mut();
            rt.reset_parameters();
            if let Some(m) = &mut self.motion { m.tick(dt); m.apply(rt); }
            self.exprs.tick(dt);
            self.exprs.apply(rt);
            // Cubism 런타임이 자동으로 하는 것 — 모션 파일에는 없다.
            self.t += dt;
            let t = self.t;
            // 모션이 쓰는 파라미터는 건드리지 않는다. 덮어쓰면 연출과 싸워서
            // 고개가 튀고 눈이 깜빡이다 만다 — 모션 하나가 8개를 쥐고 있다(실측).
            // Cubism 도 같은 규약이다: 자동 효과는 모션이 **안 쓰는** 것에만 얹는다.
            let owned = &self.motion_params;
            let put = |rt: &mut mocari::runtime::ModelRuntime, id: &str, v: f32| {
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
        // 말풍선이 따라다닐 점, 그리고 그림이 실제로 차지하는 범위.
        {
            let (mut top, mut bb) = ((0.0f32, f32::MIN), (f32::MAX, f32::MAX, f32::MIN, f32::MIN));
            for (i, m) in meshes.iter().enumerate() {
                // 안 보이는 조각은 빼야 한다 — 모델마다 캔버스를 덮는 투명 판이 하나씩
                // 있어서, 그것까지 세면 「맨 윗점」이 늘 그 판의 모서리로 굳는다.
                if !infos.get(i).map(|d| d.is_visible()).unwrap_or(true) {
                    continue;
                }
                for v in rc::vertices_from_drawable(m) {
                    let q = v.position();
                    if q[1] > top.1 {
                        top = (q[0], q[1]);
                    }
                    bb.0 = bb.0.min(q[0]);
                    bb.1 = bb.1.min(q[1]);
                    bb.2 = bb.2.max(q[0]);
                    bb.3 = bb.3.max(q[1]);
                }
            }
            // 범위는 한 번만 잡는다. 매 프레임 다시 잡으면 숨쉴 때마다 캐릭터가 커졌다
            // 작아졌다 한다.
            if self.bbox.is_none() && bb.2 > bb.0 {
                self.bbox = Some(bb);
            }
            if top.1 > f32::MIN {
                // 홱홱 튀지 않게 따라간다 — 맨 윗점은 머리카락 한 올에서 다른 올로 건너뛴다.
                let e = 1.0 - (-dt * 8.0).exp();
                self.head.0 += (top.0 - self.head.0) * e;
                self.head.1 += (top.1 - self.head.1) * e;
            }
        }
        let bufs = &self.bufs;
        // 캐릭터는 창의 아래쪽 몫에만, 그림이 차지하는 범위를 그 안에 꽉 맞춰 그린다.
        // 위는 말풍선 자리다.
        let room = ((self.h - HEADROOM) / self.h) as f32;
        let (win_w, win_h) = {
            let sz = self.win.as_ref().map(|w| w.inner_size()).unwrap_or_default();
            let sf = self.win.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
            (sz.width.max(1) as f32 / sf, sz.height.max(1) as f32 / sf)
        };
        let fit = fit_xform(self.bbox, cw, ch, win_w, win_h, room);
        let base = fit.matrix();
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
            // 할 말 — 판 없이 글자만 머리 위에 뜬다(2026-09-07 지시). 판을 두면 캐릭터
            // 위에 네모가 하나 더 얹혀 바탕화면에 얹힌 느낌이 사라진다. 밝은 바탕에서도
            // 읽히도록 글자 자체가 어두운 테두리를 두르고 온다(bubble.rs).
            if let Some((text_v, text_w, text_h)) = &self.bubble_text {
                let win = self.win.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                let sf = self.win.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
                let (sw, sh) = (win.width.max(1) as f32 / sf, win.height.max(1) as f32 / sf);
                let (px, py) = (2.0 / sw, 2.0 / sh);
                let (tw, th) = (*text_w * px, *text_h * py);

                // 머리 꼭대기를 따라간다 — 숨쉬고 고개를 돌리면 글도 함께 움직인다.
                let (hx, hy) = fit.apply(self.head);
                let x0 = (hx - tw / 2.0).clamp(-1.0, 1.0 - tw);
                let y0 = (hy + 10.0 * py + th).min(1.0);
                let (x1, y1) = (x0 + tw, y0 - th);
                let quad = [
                    V { p: [x0, y0], uv: [0.0, 0.0] },
                    V { p: [x1, y0], uv: [1.0, 0.0] },
                    V { p: [x0, y1], uv: [0.0, 1.0] },
                    V { p: [x1, y0], uv: [1.0, 0.0] },
                    V { p: [x1, y1], uv: [1.0, 1.0] },
                    V { p: [x0, y1], uv: [0.0, 1.0] },
                ];
                g.q.write_buffer(&g.bubble_vb, 0, bytemuck::cast_slice(&quad));
                let mut m = [0.0f32; 16];
                m[0] = 1.0; m[5] = 1.0; m[10] = 1.0; m[15] = 1.0;
                let u = Xf { mvp: m, mask_mtx: m, channel: [0.0; 4], opacity: 1.0, use_mask: 0.0, inverted: 0.0, _pad: 0.0 };
                g.q.write_buffer(&g.bubble_ub, 0, bytemuck::bytes_of(&u));
                let bg = bind(&g.bubble_ub, text_v, &g.dummy_view);
                rp.set_pipeline(&g.p_plain);
                rp.set_bind_group(0, &bg, &[]);
                rp.set_vertex_buffer(0, g.bubble_vb.slice(..));
                rp.draw(0..6, 0..1);
            }
        }
        g.q.submit([enc.finish()]);
        self.frames += 1;
        if self.frames == self.shot_at {
            if let Some(path) = self.shot_path.clone() {
                save_shot(g, &frame.texture, &path);
            }
        }
        frame.present();
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

/// model3.json 이 적어 둔 모션 중 평소에 돌릴 것 하나. 모션 파일 이름은 모델마다 다르므로
/// 이름을 박아 두면 모델을 갈아 끼우는 순간 조용히 「모션 없음」이 된다 — 목록에서 고른다.
/// `Idle` 무리를 먼저 보고, 없으면 아무 무리의 첫 칸.
/// 지금 화면을 PNG 로 뜬다 — 투명 배경 그대로라 캐릭터만 남는다. macOS 의
/// `screencapture` 는 권한이 막혀 안 되고, 펫은 창이 따로라 kasaterm 의 캡처도 못 미친다.
fn save_shot(g: &Gfx, tex: &wgpu::Texture, path: &str) {
    let (w, h) = (tex.width(), tex.height());
    // 되읽기 버퍼는 줄마다 256 바이트로 맞춰야 한다(wgpu 규약).
    let row = ((w * 4).div_ceil(256)) * 256;
    let buf = g.dev.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = g.dev.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        tex.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    g.q.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = g.dev.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    let src = buf.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let line = &src[(y * row) as usize..(y * row + w * 4) as usize];
        // 서피스는 BGRA 다 — PNG 는 RGBA 라 두 칸을 맞바꾼다.
        for px in line.chunks_exact(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        let _ = y;
    }
    drop(src);
    buf.unmap();
    let Ok(f) = std::fs::File::create(path) else { return };
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    if let Ok(mut wr) = e.write_header() {
        let _ = wr.write_image_data(&out);
    }
    eprintln!("캡처: {path}");
}



/// model3.json 이 적어 둔 모션 파일 전부(무리 순서대로). 공식 샘플은 무리 이름이
/// `Idle`·`TapBody` 뿐이라 「Busy 모션」 같은 이름으로는 못 고른다 — 자리로 고른다.
fn motion_files(model3: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = model3.parent().unwrap_or(std::path::Path::new("."));
    let Ok(t) = std::fs::read_to_string(model3) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(g) = v.get("FileReferences").and_then(|f| f.get("Motions")).and_then(|m| m.as_object()) {
        for (_, arr) in g {
            for e in arr.as_array().into_iter().flatten() {
                if let Some(f) = e.get("File").and_then(|f| f.as_str()) {
                    out.push(dir.join(f));
                }
            }
        }
    }
    out
}

fn expression_files(model3: &std::path::Path) -> Vec<std::path::PathBuf> {
    let dir = model3.parent().unwrap_or(std::path::Path::new("."));
    let Ok(t) = std::fs::read_to_string(model3) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { return Vec::new() };
    v.get("FileReferences")
        .and_then(|f| f.get("Expressions"))
        .and_then(|e| e.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("File").and_then(|f| f.as_str()))
                .map(|f| dir.join(f))
                .collect()
        })
        .unwrap_or_default()
}

/// 그 폴더가 쥔 model3.json. 캐릭터 폴더인지 가르는 기준이기도 하다.
fn model3_in(dir: &std::path::Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".model3.json"))
        .map(|p| p.to_string_lossy().into_owned())
}

/// 같은 자리에서 다른 모델로 다시 시작한다. pid 가 그대로라 하단바 칩이 계속 켜짐으로 보인다.
#[cfg(unix)]
fn exec_self(model: &str) {
    use std::ffi::CString;
    let Ok(exe) = std::env::current_exe() else { return };
    let (Ok(a0), Ok(a1)) = (
        CString::new(exe.to_string_lossy().as_bytes()),
        CString::new(model.as_bytes()),
    ) else {
        return;
    };
    let argv = [a0.as_ptr(), a1.as_ptr(), std::ptr::null()];
    unsafe { libc::execv(a0.as_ptr(), argv.as_ptr()) };
}

#[cfg(not(unix))]
fn exec_self(model: &str) {
    if let Ok(exe) = std::env::current_exe() {
        if std::process::Command::new(exe).arg(model).spawn().is_ok() {
            std::process::exit(0);
        }
    }
}

fn idle_motion(model3: &str) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(model3).ok()?).ok()?;
    let groups = v.get("FileReferences")?.get("Motions")?.as_object()?;
    let pick = groups
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("idle"))
        .or_else(|| groups.iter().next())?;
    let f = pick.1.as_array()?.first()?.get("File")?.as_str()?;
    Some(f.to_string())
}


fn main() {
    let path = std::env::args().nth(1).unwrap();
    let model = mocari::assets::load_model_runtime(&path).expect("모델");
    let dir = std::path::Path::new(&path).parent().unwrap().to_path_buf();
    let file = std::env::args()
        .nth(2)
        .or_else(|| idle_motion(&path))
        .unwrap_or_default();
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
    let model3 = std::path::Path::new(&path).to_path_buf();
    let motion_files = motion_files(&model3);
    let expr_files = expression_files(&model3);
    eprintln!("모션 파일: {file}");
    eprintln!("모션 로드: {}", if motion.is_some() { "성공" } else { "실패" });
    // 캐릭터 폴더(`<pet>/<이름>/<이름>.model3.json`)에서 왔으면 그 위가 펫 자리다.
    // env 로 모델을 직접 준 검증 실행은 그 구조가 아니므로 자리 저장을 안 한다.
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let pet_dir = dir.parent().filter(|p| p.join("current").exists()).map(|p| p.to_path_buf());
    let (mut x, mut y, mut scale) = (60.0_f64, 80.0_f64, 1.0_f32);
    if let Some(t) = pet_dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("state.json")).ok()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
            x = v.get("x").and_then(|n| n.as_f64()).unwrap_or(x);
            y = v.get("y").and_then(|n| n.as_f64()).unwrap_or(y);
            scale = v.get("scale").and_then(|n| n.as_f64()).unwrap_or(scale as f64) as f32;
        }
    }

    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Poll);
    let mut app = App { win: None, gfx: None,
        x, y, w: 420.0, h: 600.0 + HEADROOM, scale,
        alpha: wgpu::CompositeAlphaMode::Auto, cursor: (0.0, 0.0), pet_dir, name, last_click: None,
        mood: board::Mood::Idle, say: String::new(), board_seen: None,
        board_polled: std::time::Instant::now(), stirred: std::time::Instant::now(),
        bubble_text: None, head: (0.0, 0.0), bbox: None,
        motion_files, expr_files, exprs: mocari::expression::ExpressionManager::new(), bufs: Vec::new(), ubs: Vec::new(), look: (0.0, 0.0), look_now: (0.0, 0.0), motion_params,
        model, motion, last: std::time::Instant::now(), t: 0.0, fps_t: std::time::Instant::now(), fps_n: 0, dts: Vec::new(), frames: 0,
        shot_path: std::env::var("KASAPET_SHOT").ok(),
        shot_at: std::env::var("KASAPET_SHOT_FRAME").ok().and_then(|v| v.parse().ok()).unwrap_or(120) };
    el.run_app(&mut app).unwrap();
}

/// 그림을 창의 아래쪽 몫에 꽉 맞추는 변환.
#[derive(Clone, Copy)]
struct Fit {
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
}

impl Fit {
    fn matrix(self) -> [f32; 16] {
        let mut m = [0.0f32; 16];
        m[0] = self.sx;
        m[5] = self.sy;
        m[10] = 1.0;
        m[12] = self.tx;
        m[13] = self.ty;
        m[15] = 1.0;
        m
    }

    fn apply(self, p: (f32, f32)) -> (f32, f32) {
        (p.0 * self.sx + self.tx, p.1 * self.sy + self.ty)
    }
}

/// `room` 은 창에서 캐릭터가 쓸 세로 몫(나머지 위쪽은 말풍선 자리).
///
/// ⚠️ **배율은 픽셀로 재고 NDC 로 옮긴다.** NDC 는 축마다 따로 정규화되므로 x·y 에 같은
/// NDC 배율을 주면 창 비율만큼 눌린다 — 세로로 긴 창에서는 캐릭터가 홀쭉해진다.
fn fit_xform(
    bbox: Option<(f32, f32, f32, f32)>,
    cw: f32,
    ch: f32,
    sw: f32,
    sh: f32,
    room: f32,
) -> Fit {
    // 범위를 아직 못 잡았으면(첫 프레임) 모델이 선언한 캔버스로 친다.
    let (x0, y0, x1, y1) = bbox.unwrap_or((-cw / 2.0, -ch / 2.0, cw / 2.0, ch / 2.0));
    let (w, h) = ((x1 - x0).max(1e-3), (y1 - y0).max(1e-3));
    // 모델 한 칸이 화면에서 몇 픽셀인가 — 가로세로 같은 값이라 비율이 안 망가진다.
    let ppu = (sw / w).min(sh * room / h);
    let (sx, sy) = (ppu * 2.0 / sw, ppu * 2.0 / sh);
    Fit {
        sx,
        sy,
        tx: -(x0 + x1) / 2.0 * sx,
        // 발이 창 바닥에 닿게 — 캐릭터가 공중에 뜨면 바탕화면 펫으로 안 보인다.
        ty: -1.0 - y0 * sy,
    }
}
