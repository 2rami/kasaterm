//! macOS 창 레이어 손질 — Display P3 루트 Metal 레이어, 크기 조절 중 내용 고정, 레이어 배율 맞추기.
//! 다른 플랫폼에서는 같은 이름의 빈 함수가 선다.

#[cfg(target_os = "macos")]
use anyhow::{Context, Result};
#[cfg(target_os = "macos")]
use raw_window_handle::HasWindowHandle;
use winit::window::Window;

/// NSView 가 창의 콘텐츠 영역을 **꽉 채우는지** 확인하고, 작으면 다시 채운다.
/// 고쳤으면 `true`.
///
/// 사용자가 큰 모니터에서 본 화면(창 1510x950 안에 UI 가 754x472 로 온전히
/// 축소돼 구석에 붙고, 빈 영역엔 우리 배경색이 아닌 NSWindow 기본색)이 바로
/// 이 상태다. 뷰가 작아지면 그 아래(레이어·`inner_size()`·스왑체인)가 전부
/// 사이좋게 작아지므로 **앱 내부에선 아무 모순이 안 보인다** — 어긋난 건 창과
/// 뷰 사이뿐이라, 자기 크기만 들여다보는 코드로는 영영 못 잡는다. 그래서 창
/// 쪽(`contentRectForFrameRect:`)을 기준으로 삼는다.
///
/// 스왑체인만 어긋난 경우와는 증상이 다르다(그쪽은 UI 가 **잘린다**).
/// `KASATERM_FORCE_SURFACE_HALF_MS` 로 둘을 갈라 실측해 둔 구분이다.
#[cfg(target_os = "macos")]
pub fn ensure_view_fills_window(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let ns_window: *mut AnyObject = msg_send![ns_view, window];
        if ns_window.is_null() {
            return false;
        }
        let wf: NSRect = msg_send![ns_window, frame];
        let content: NSRect = msg_send![ns_window, contentRectForFrameRect: wf];
        let vf: NSRect = msg_send![ns_view, frame];
        // 미니마이즈/화면 밖 등으로 0 이 나올 때 뷰를 0 으로 만들지 않는다.
        if !(content.size.width > 1.0 && content.size.height > 1.0) {
            return false;
        }
        if (content.size.width - vf.size.width).abs() < 1.0
            && (content.size.height - vf.size.height).abs() < 1.0
            && vf.origin.x.abs() < 1.0
            && vf.origin.y.abs() < 1.0
        {
            return false;
        }
        let fixed = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(content.size.width, content.size.height),
        );
        let _: () = msg_send![ns_view, setFrame: fixed];
        eprintln!(
            "[viewfit] 뷰가 창보다 작았다 — {:.0}x{:.0}@({:.0},{:.0}) → {:.0}x{:.0} 로 복구",
            vf.size.width, vf.size.height, vf.origin.x, vf.origin.y,
            content.size.width, content.size.height
        );
        true
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_view_fills_window(_window: &Window) -> bool {
    false
}

/// 레이어의 backing scale(`contentsScale`)이 창의 현재 scale 과 맞는지 확인하고,
/// 어긋나면 맞춘다. 고쳤으면 `true`.
///
/// 모니터를 옮기면 winit 의 `scale_factor()` 도 drawable 도 새 화면을 따라가는데
/// **레이어의 `contentsScale` 만 창을 만들 때 박힌 값에 영원히 머문다** — 내장↔외부를
/// 세 번 오가며 실측해도 cs 는 초기값 그대로였다. 그러면 레이어는 "이 넓이를 cs 배
/// 픽셀로 채워라" 라고 기대하는데 drawable 은 새 scale 기준이라, 2→1(내장→외부)
/// 이동에선 텍스처가 레이어 좌상단 1/4 에만 그려지고 나머지는 우리가 안 그린
/// NSWindow 기본색으로 남는다. 사용자가 본 "큰 모니터로 옮기면 화면이 구석에 절반
/// 크기로 처박힘" 이 이것이고, 맥북으로 되돌리면 멀쩡한 건 고쳐져서가 아니라 cs 가
/// 원래 맞던 화면으로 돌아왔을 뿐이다.
///
/// 렌더버그 카탈로그 39번이 이 자리를 "죽은 가설" 로 폐기했던 건 반증 실험이 cs 를
/// **4.0 이라는 아무 화면과도 안 맞는 값**으로 강제해 본 것이었기 때문이다. 문제는
/// cs 의 절대값이 아니라 cs 와 drawable 이 서로 어긋나는 것이라, 틀린 값으로 흔들면
/// 아무 일도 안 일어나고 맞는 값으로 맞춰야 낫는다.
#[cfg(target_os = "macos")]
pub fn ensure_layer_scale_matches(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let layer: *mut AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            return false;
        }
        let cur: f64 = msg_send![layer, contentsScale];
        let want = window.scale_factor();
        // 화면이 없거나 축소 중이면 0 이 나올 수 있다 — 그 값으로 레이어를 망치지 않는다.
        if !(want > 0.0) || (cur - want).abs() < 0.01 {
            return false;
        }
        let _: () = msg_send![layer, setContentsScale: want];
        eprintln!("[layerscale] 레이어 backing scale 이 창과 어긋났다 — {cur} → {want} 로 맞춤");
        true
    }
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_layer_scale_matches(_window: &Window) -> bool {
    false
}

#[cfg(target_os = "macos")]
pub unsafe fn patch_metal_layer_gravity(window: &Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;
    use raw_window_handle::RawWindowHandle;

    let Ok(handle) = window.window_handle() else { return; };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return; };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

    let root_layer: *mut AnyObject = msg_send![ns_view, layer];
    if root_layer.is_null() { return; }

    // wgpu attaches its drawing layer (WgpuObserverLayer wrapping a
    // CAMetalLayer) as a sublayer of the NSView's backing layer — so the
    // contents we need to anchor with gravity live on the SUBLAYER, not on
    // the NSView's root layer. Walk the tree and pin gravity on every
    // descendant we find.
    let gravity = NSString::from_str("topLeft");
    fn patch_recursive(layer: *mut objc2::runtime::AnyObject, gravity: &objc2_foundation::NSString) {
        use objc2::msg_send;
        unsafe {
            let _: () = msg_send![layer, setContentsGravity: gravity];
            let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
            if subs.is_null() { return; }
            let n: usize = msg_send![subs, count];
            for i in 0..n {
                let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                if !s.is_null() {
                    patch_recursive(s, gravity);
                }
            }
        }
    }
    patch_recursive(root_layer, &gravity);
    eprintln!("[live-resize-probe] patched gravity recursively from root layer");

    // NSWindow-level colorspace. wgpu attaches its CAMetalLayer as a
    // SUBLAYER (sugarloaf replaces the view's layer entirely — that's
    // why their P3 tag stuck and ours didn't). For sublayer-based
    // setups the window's `colorSpace` is what macOS color-manages
    // against; setting it propagates Display P3 to everything inside.
    let ns_window: *mut AnyObject = msg_send![ns_view, window];
    if !ns_window.is_null() {
        if let Some(ns_cs_cls) = objc2::runtime::AnyClass::get(c"NSColorSpace") {
            let p3: *mut AnyObject = msg_send![ns_cs_cls, displayP3ColorSpace];
            if !p3.is_null() {
                let _: () = msg_send![ns_window, setColorSpace: p3];
                eprintln!("[gpu] NSWindow colorSpace → Display P3");
            }
        }
    }

    // Display P3 on CAMetalLayer. Doesn't modify source colours — just
    // tells macOS to interpret the same sRGB-encoded bytes as P3 at
    // scan-out. On Retina P3 panels the green (and red, blue) primaries
    // reach the wider P3 gamut → noticeably punchier diff bg highlights
    // / Claude Code colour chips. We had this once, removed it for fear
    // of "altering the terminal", but it's the layer-level setting
    // ghostty / iTerm2 use by default; the byte values stay untouched.
    patch_p3_colorspace_safe(root_layer);

    // NSViewLayerContentsRedrawPolicy: 2 = .duringViewResize. Default
    // (.onSetNeedsDisplay) lets AppKit skip paint during the live-resize
    // tracking loop, which is what makes the grid lag behind the frame.
    let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
    // NSViewLayerContentsPlacement: 9 = .topLeft — mirrors the layer gravity
    // so AppKit's own resize-time scaling doesn't stretch contents either.
    let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];
}

/// Create a fresh CAMetalLayer, install it as the NSView's root layer,
/// tag it Display P3, and return the raw pointer. Used by the
/// `KASATERM_P3_ROOT=1` opt-in path: feeding this pointer to
/// `SurfaceTargetUnsafe::CoreAnimationLayer` makes wgpu reuse our layer
/// rather than create a sublayer-attached one (the macOS-color-management
/// blocker described in reference_kasaterm_color_pipeline).
///
/// Returns the layer pointer cast to `*mut c_void` — what wgpu wants.
#[cfg(target_os = "macos")]
pub unsafe fn install_root_p3_layer(
    window: &winit::window::Window,
    scale: f32,
) -> Result<*mut std::ffi::c_void> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;

    unsafe {
        let handle = window.window_handle().context("no window handle")?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            anyhow::bail!("not an AppKit handle");
        };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;

        // Fresh CAMetalLayer instance — `[[CAMetalLayer alloc] init]`.
        let metal_cls = objc2::runtime::AnyClass::get(c"CAMetalLayer")
            .context("CAMetalLayer class missing")?;
        let layer_obj: *mut AnyObject = msg_send![metal_cls, alloc];
        let layer_ptr: *mut AnyObject = msg_send![layer_obj, init];
        if layer_ptr.is_null() {
            anyhow::bail!("CAMetalLayer init returned nil");
        }

        // setFrame on the layer requires the NSRect encode trait we
        // don't bring in here — and wgpu's `surface.configure()` calls
        // `setDrawableSize` later anyway, so skipping the initial frame
        // is harmless. Just pin the backing scale.
        let _: () = msg_send![layer_ptr, setContentsScale: scale as f64];
        // Anchor content to top-left during live resize (same as
        // patch_metal_layer_gravity for the legacy path).
        let topleft = objc2_foundation::NSString::from_str("topLeft");
        let _: () = msg_send![layer_ptr, setContentsGravity: &*topleft];

        // P3 colorspace tag — cached because CGColorSpace is expensive.
        static CS: OnceLock<usize> = OnceLock::new();
        let cs = *CS.get_or_init(|| {
            #[link(name = "CoreGraphics", kind = "framework")]
            unsafe extern "C" {
                fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
                static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            }
            let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            p as usize
        });
        if cs != 0 {
            let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        }

        // Install as the NSView's root layer (layer-hosting view).
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // Match the legacy patch_metal_layer_gravity: redraw on resize,
        // keep contents top-left during live drag.
        let _: () = msg_send![ns_view, setLayerContentsRedrawPolicy: 2_isize];
        let _: () = msg_send![ns_view, setLayerContentsPlacement: 9_isize];

        eprintln!(
            "[gpu] installed root P3 metal layer {:p} on NSView {:p}",
            layer_ptr, ns_view
        );
        Ok(layer_ptr as *mut std::ffi::c_void)
    }
}

/// Promote wgpu's CAMetalLayer (created as a sublayer by `layer_observer`)
/// to be the NSView's root layer. Without this, macOS color-manages
/// the parent root and silently ignores the sublayer's `colorspace`
/// tag, so Display P3 never takes effect (Color Meter reads pure sRGB).
/// Sugarloaf does this directly because it owns the layer creation.
#[cfg(target_os = "macos")]
pub unsafe fn promote_metal_layer_to_root(
    window: &winit::window::Window,
    surface: &wgpu::Surface<'static>,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    unsafe {
        let Ok(handle) = window.window_handle() else { return };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
        let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        // setLayer: requires the view to want a layer.
        let _: () = msg_send![ns_view, setLayer: layer_ptr];
        let _: () = msg_send![ns_view, setWantsLayer: true];
        // P3 colorspace stays sticky only when EDR is enabled — on Apple
        // Silicon Mini-LED panels macOS color-manages SDR content to the
        // sRGB primary subspace of the display unless wantsEDR is on.
        // Use respondsToSelector to avoid the abort we hit earlier on
        // macOS 26 when calling it via the wrong object.
        let edr_sel = objc2::sel!(setWantsExtendedDynamicRangeContent:);
        let responds: bool = msg_send![layer_ptr, respondsToSelector: edr_sel];
        if responds {
            let _: () = msg_send![layer_ptr, setWantsExtendedDynamicRangeContent: true];
            eprintln!("[gpu] EDR enabled on render layer");
        }
        eprintln!("[gpu] promoted wgpu CAMetalLayer to NSView root layer");
    }
}

/// Apply P3 colorspace through wgpu-hal directly — the actual render
/// layer wgpu owns, not whatever sublayer we walked the NSView tree
/// looking for. Without this, the layer-walk approach silently fails
/// (Color Meter still reads 255,0,0 for a pure-red printf).
#[cfg(target_os = "macos")]
pub fn apply_p3_via_hal(surface: &wgpu::Surface<'static>) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use std::sync::OnceLock;
    static CS: OnceLock<usize> = OnceLock::new();
    let cs = *CS.get_or_init(|| unsafe {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        let p = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
        p as usize
    });
    if cs == 0 { return; }
    unsafe {
        let hal_surface_opt = surface.as_hal::<wgpu_hal::api::Metal>();
        let Some(hal_surface) = hal_surface_opt else { return };
        let layer_lock = hal_surface.render_layer().lock();
        // metal::MetalLayerRef IS the CAMetalLayer Obj-C object — its
        // `&Ref` IS the pointer. Cast through *const () to drop the
        // type info safely.
        let layer_ref = layer_lock.as_ref();
        let layer_ptr: *mut AnyObject = layer_ref as *const _ as *mut AnyObject;
        let _: () = msg_send![layer_ptr, setColorspace: cs as *mut std::ffi::c_void];
        if std::env::var_os("KASATERM_COLORSPACE_DEBUG").is_some() {
            let applied: *mut AnyObject = msg_send![layer_ptr, colorspace];
            eprintln!(
                "[gpu] HAL P3 set on render_layer={:p} applied={}",
                layer_ptr,
                !applied.is_null()
            );
        }
    }
}

/// Per-frame P3 colorspace re-application via NSView layer walk. Kept as
/// a belt-and-braces — wgpu-hal path is the real fix.
#[cfg(target_os = "macos")]
pub unsafe fn reapply_p3(window: &winit::window::Window) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    use std::sync::OnceLock;
    static CACHED: OnceLock<(usize, usize)> = OnceLock::new(); // (layer_ptr, cs_ptr)
    let entry = CACHED.get_or_init(|| {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
        }
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceDisplayP3);
            let Ok(handle) = window.window_handle() else { return (0, 0) };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return (0, 0) };
            let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
            let root_layer: *mut AnyObject = msg_send![ns_view, layer];
            // Walk to find the first CAMetalLayer-subclass descendant.
            let Some(metal_cls) = objc2::runtime::AnyClass::get(c"CAMetalLayer") else {
                return (0, 0);
            };
            fn find(l: *mut AnyObject, cls: &objc2::runtime::AnyClass) -> *mut AnyObject {
                unsafe {
                    let is_metal: bool = msg_send![l, isKindOfClass: cls];
                    if is_metal { return l; }
                    let subs: *mut AnyObject = msg_send![l, sublayers];
                    if !subs.is_null() {
                        let n: usize = msg_send![subs, count];
                        for i in 0..n {
                            let s: *mut AnyObject = msg_send![subs, objectAtIndex: i];
                            if !s.is_null() {
                                let r = find(s, cls);
                                if !r.is_null() { return r; }
                            }
                        }
                    }
                    std::ptr::null_mut()
                }
            }
            let metal_layer = find(root_layer, metal_cls);
            (metal_layer as usize, cs as usize)
        }
    });
    let (layer_ptr, cs_ptr) = *entry;
    if layer_ptr == 0 || cs_ptr == 0 { return; }
    let layer = layer_ptr as *mut AnyObject;
    let cs = cs_ptr as *mut std::ffi::c_void;
    let _: () = unsafe { msg_send![layer, setColorspace: cs] };
}

/// Walks the layer tree and sets every CAMetalLayer descendant's
/// colorspace to Display P3 via direct CoreGraphics FFI. Skips any
/// non-CAMetalLayer (CALayer doesn't respond to `setColorspace:` on
/// older OS versions and the previous "patch every layer" version
/// aborted there). Returns silently on any failure — colours stay
/// sRGB rather than crashing the process.
#[cfg(target_os = "macos")]
pub fn patch_p3_colorspace_safe(root_layer: *mut objc2::runtime::AnyObject) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        // ExtendedDisplayP3 (vs plain DisplayP3): the "extended" variant
        // accepts encoded values outside [0,1] mapping to HDR-bright
        // colours. Even on a Bgra8Unorm framebuffer (which clamps), the
        // layer's intent telegraphs to the macOS compositor that we want
        // the panel's widest available gamut. Ghostty / iTerm2 both
        // settle on this when an EDR display is detected.
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
            fn CGColorSpaceRelease(cs: *mut std::ffi::c_void);
            static kCGColorSpaceDisplayP3: *const std::ffi::c_void;
            static kCGColorSpaceExtendedDisplayP3: *const std::ffi::c_void;
        }
        // env override KASATERM_COLORSPACE=p3|extended-p3|disabled
        let cs_name = std::env::var("KASATERM_COLORSPACE")
            .unwrap_or_else(|_| "p3".to_string());
        let cs_ref: *const std::ffi::c_void = match cs_name.as_str() {
            "disabled" => return,
            "extended-p3" => kCGColorSpaceExtendedDisplayP3,
            _ => kCGColorSpaceDisplayP3,
        };
        let cs = CGColorSpaceCreateWithName(cs_ref);
        if cs.is_null() {
            return;
        }
        let Some(metal_class) = AnyClass::get(c"CAMetalLayer") else {
            CGColorSpaceRelease(cs);
            return;
        };
        fn walk(
            layer: *mut objc2::runtime::AnyObject,
            cs: *mut std::ffi::c_void,
            metal_class: &AnyClass,
        ) -> usize {
            unsafe {
                let mut hits = 0usize;
                let is_metal: bool = msg_send![layer, isKindOfClass: metal_class];
                if is_metal {
                    let _: () = msg_send![layer, setColorspace: cs];
                    hits += 1;
                }
                let subs: *mut objc2::runtime::AnyObject = msg_send![layer, sublayers];
                if subs.is_null() {
                    return hits;
                }
                let n: usize = msg_send![subs, count];
                for i in 0..n {
                    let s: *mut objc2::runtime::AnyObject = msg_send![subs, objectAtIndex: i];
                    if !s.is_null() {
                        hits += walk(s, cs, metal_class);
                    }
                }
                hits
            }
        }
        let hits = walk(root_layer, cs, metal_class);
        // Sugarloaf's defensive pattern: never release the colorspace
        // we just handed to the layer. The CA property is documented to
        // retain on set, but if Apple ever changes that semantics our
        // colorspace would silently drop and the layer falls back to
        // sRGB — exactly the "set returned ok but colours look wrong"
        // symptom. We create one per process, so the leak is fine.
        // (See sugarloaf-0.4.4/src/context/metal.rs.)
        // `cs` is a *mut c_void (Copy) — `mem::forget` on it is a no-op;
        // we just want to suppress unused-result warnings. The actual
        // retain happens at the setColorspace: msg_send above.
        let _ = cs;
        eprintln!("[gpu] CAMetalLayer colorspace → {cs_name} ({hits} layer(s) tagged)");
    }
}

/// True while AppKit's live-resize tracking loop owns the window — the user
/// is dragging an edge. ghostty's resize trick depends on knowing this:
/// during live resize we leave the CAMetalLayer's drawableSize alone (no
/// surface.configure, no render) so the layer keeps its last painted
/// contents, and gravity=topLeft anchors that to the top-left while AppKit
/// stretches the bounds. The newly revealed area shows the clear colour
/// instead of stretched stale pixels.
#[cfg(target_os = "macos")]
pub fn is_in_live_resize(window: &Window) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::RawWindowHandle;
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return false;
    };
    let ns_view = h.ns_view.as_ptr() as *mut AnyObject;
    unsafe {
        let r: bool = msg_send![ns_view, inLiveResize];
        r
    }
}

#[cfg(not(target_os = "macos"))]
pub fn is_in_live_resize(_window: &Window) -> bool {
    false
}

/// Run `f` inside a CATransaction with implicit animations disabled. AppKit
/// hangs a layer animation on bounds jumps (zoom / maximize is the worst
/// case) and lets stale contents interpolate to the new bounds — gravity
/// alone can't fix that mid-animation. Wrapping the resize + render kills
/// the animation so the new frame is what AppKit composites.
#[cfg(target_os = "macos")]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    unsafe {
        let Some(class) = AnyClass::get(c"CATransaction") else {
            f();
            return;
        };
        let _: () = msg_send![class, begin];
        let _: () = msg_send![class, setDisableActions: true];
        f();
        let _: () = msg_send![class, commit];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn with_disabled_layer_actions<F: FnOnce()>(f: F) {
    f();
}
