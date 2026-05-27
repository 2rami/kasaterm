//! Minimal PoC: NSView root CAMetalLayer + Display P3 + draw a red rect.
//!
//! Goal: verify that on macOS 26.3, a root-installed CAMetalLayer with
//! `setColorspace: DisplayP3` actually causes pure-red (byte 255,0,0)
//! to display as P3 pure-red (which screencapture saves as ~234,51,35
//! in sRGB PNG). If this PoC works, then kasaterm's failure on the same
//! pattern is wgpu's fault — wgpu is doing something between init and
//! present that drops the P3 mapping. If this PoC also fails, the
//! pattern itself doesn't work and we need a different approach.
//!
//! NO wgpu involved. Pure objc2 + metal crate.

use metal::foreign_types::ForeignType;
use metal::*;
use objc2::msg_send;
use objc2::runtime::AnyObject;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::OnceLock;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowId};

struct App {
    window: Option<Window>,
    layer: Option<MetalLayer>,
    device: Option<Device>,
    queue: Option<CommandQueue>,
    frames: u32,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("p3-poc")
            .with_inner_size(winit::dpi::LogicalSize::new(400.0, 200.0))
            .with_position(winit::dpi::LogicalPosition::new(100.0, 100.0));
        let window = event_loop.create_window(attrs).expect("window");
        // Self-capture: take screencapture of our own window after N ms.
        if let Ok(ms) = std::env::var("KASAPOC_AUTOCAP") {
            if let Ok(ms_u) = ms.parse::<u64>() {
                let scale = window.scale_factor();
                let pos = window.outer_position().expect("outer_position");
                let size = window.outer_size();
                let x_logical = (pos.x as f64 / scale).round() as i32;
                let y_logical = (pos.y as f64 / scale).round() as i32;
                let w_logical = (size.width as f64 / scale).round() as i32;
                let h_logical = (size.height as f64 / scale).round() as i32;
                let rect = format!("{x_logical},{y_logical},{w_logical},{h_logical}");
                let path = std::env::var("KASAPOC_AUTOCAP_PATH")
                    .unwrap_or_else(|_| "/tmp/p3-poc.png".to_string());
                eprintln!(
                    "[poc] window outer = physical({}, {}, {}, {}) / logical_rect={} scale={}",
                    pos.x, pos.y, size.width, size.height, rect, scale
                );
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms_u));
                    let _ = std::process::Command::new("screencapture")
                        .args(["-x", "-t", "png", "-R", &rect, &path])
                        .status();
                    eprintln!("[poc] saved {path} (rect={rect})");
                    std::process::exit(0);
                });
            }
        }

        let device = Device::system_default().expect("metal device");
        let layer = MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        layer.set_presents_with_transaction(false);
        layer.set_framebuffer_only(false);

        // Display P3 tag (the whole point of the PoC). objc2's msg_send!
        // refuses the call because the selector's recorded type encoding
        // wants a `^{CGColorSpace=}` (opaque CGColorSpace*) and we'd be
        // passing `*mut c_void`. Bypass via raw `objc_msgSend` — same wire
        // call, no Rust-side encoding check.
        unsafe {
            #[link(name = "CoreGraphics", kind = "framework")]
            unsafe extern "C" {
                fn CGColorSpaceCreateWithName(
                    name: *const std::ffi::c_void,
                ) -> *mut std::ffi::c_void;
                static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            }
            if std::env::var_os("KASAPOC_NO_P3_TAG").is_some() {
                eprintln!("[poc] KASAPOC_NO_P3_TAG set — SKIPPING layer setColorspace");
            } else {
                let cs = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
                if !cs.is_null() {
                    let layer_ptr: *mut AnyObject = layer.as_ptr() as *mut AnyObject;
                    let sel = objc2::sel!(setColorspace:);
                    type MsgSendCs = unsafe extern "C" fn(
                        *mut AnyObject,
                        objc2::runtime::Sel,
                        *mut std::ffi::c_void,
                    );
                    let send: MsgSendCs =
                        std::mem::transmute(objc2::ffi::objc_msgSend as *const ());
                    send(layer_ptr, sel, cs);
                    eprintln!("[poc] layer setColorspace P3 done (raw msgSend)");
                } else {
                    eprintln!("[poc] WARNING: CGColorSpaceCreateWithName returned null");
                }
            }
        }

        // Install layer as the NSView's ROOT layer.
        unsafe {
            let handle = window.window_handle().unwrap();
            let RawWindowHandle::AppKit(h) = handle.as_raw() else {
                panic!("not appkit");
            };
            let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
            let layer_ptr: *mut AnyObject = layer.as_ptr() as *mut AnyObject;
            let _: () = msg_send![ns_view, setLayer: layer_ptr];
            let _: () = msg_send![ns_view, setWantsLayer: true];
            eprintln!("[poc] installed root metal layer on NSView");
        }

        let queue = device.new_command_queue();

        // Drawable size — without this the layer has 0x0 and next_drawable
        // returns None forever. wgpu's surface.configure does this for us
        // in the kasaterm path; here we own it.
        let size = window.inner_size();
        layer.set_drawable_size(core_graphics_types::geometry::CGSize::new(
            size.width as f64,
            size.height as f64,
        ));
        eprintln!(
            "[poc] layer drawable_size = ({}, {})",
            size.width, size.height
        );
        // Trigger first redraw — winit 0.30 doesn't auto-redraw on resume.
        window.request_redraw();

        self.device = Some(device);
        self.queue = Some(queue);
        self.layer = Some(layer);
        self.window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.draw();
                self.frames += 1;
                if self.frames > 300 {
                    event_loop.exit();
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl App {
    fn draw(&self) {
        let layer = self.layer.as_ref().unwrap();
        let queue = self.queue.as_ref().unwrap();
        if self.frames < 3 {
            eprintln!("[poc] draw #{} requesting next_drawable", self.frames);
        }

        let Some(drawable) = layer.next_drawable() else {
            eprintln!("[poc] draw #{}: next_drawable=None", self.frames);
            return;
        };
        if self.frames < 3 {
            eprintln!("[poc] draw #{} got drawable", self.frames);
        }

        // Clear to either pure-sRGB red or sugarloaf-style P3-encoded red,
        // depending on KASAPOC_MODE. The matrix below is exactly what
        // sugarloaf's `prepare_output_rgb(1.0, 0, 0)` produces — sRGB pure
        // red, walked through srgb_to_linear → sRGB→P3 Bradford → linear_to_srgb,
        // so a DisplayP3-tagged layer can store it and the compositor
        // shows P3 pure red. Screencapture reading that back as sRGB PNG
        // gives ~234,51,35 (the value sugarloaf measures in memory).
        let mode = std::env::var("KASAPOC_MODE").unwrap_or_else(|_| "raw".to_string());
        let (r, g, b) = if mode == "p3" {
            // sugarloaf prepare_output_rgb(1,0,0) with input_colorspace=0 (sRGB):
            //   linear (1,0,0) → matrix → (0.822, 0.033, 0.017) → linear_to_srgb
            //   linear_to_srgb(0.822) ≈ 0.917  → byte 234
            //   linear_to_srgb(0.033) ≈ 0.205  → byte 52
            //   linear_to_srgb(0.017) ≈ 0.139  → byte 35
            if self.frames == 0 {
                eprintln!("[poc] mode=p3 (sugarloaf-style P3-encoded red)");
            }
            (0.917_f64, 0.205, 0.139)
        } else {
            if self.frames == 0 {
                eprintln!("[poc] mode=raw (sRGB pure red)");
            }
            (1.0_f64, 0.0, 0.0)
        };
        let render_pass = RenderPassDescriptor::new();
        let color_attachment = render_pass.color_attachments().object_at(0).unwrap();
        color_attachment.set_texture(Some(drawable.texture()));
        color_attachment.set_load_action(MTLLoadAction::Clear);
        color_attachment.set_store_action(MTLStoreAction::Store);
        color_attachment.set_clear_color(MTLClearColor::new(r, g, b, 1.0));

        let cmd_buf = queue.new_command_buffer();
        let encoder = cmd_buf.new_render_command_encoder(render_pass);
        encoder.end_encoding();
        cmd_buf.present_drawable(drawable);
        cmd_buf.commit();
    }
}

fn main() {
    static APP_CSPACE_INIT: OnceLock<()> = OnceLock::new();
    APP_CSPACE_INIT.get_or_init(|| {
        eprintln!("[poc] starting — KASAPOC_AUTOCAP=N for self-capture in N ms");
    });

    // Self-capture scheduled inside `resumed()` once we know window bounds.

    let event_loop = EventLoop::new().unwrap();
    let mut app = App {
        window: None,
        layer: None,
        device: None,
        queue: None,
        frames: 0,
    };
    event_loop.run_app(&mut app).unwrap();
}
