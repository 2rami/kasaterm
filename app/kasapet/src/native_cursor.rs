use std::ffi::c_void;

use objc2_foundation::{NSNumber, NSString};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowListCreate(options: u32, relative_to: u32) -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// Native mouse hit testing excludes a click-through pet. Compare the hit window
/// against the windows above the pet instead, without changing input routing.
pub(crate) fn is_frontmost_at_cursor(win: &winit::window::Window) -> bool {
    use objc2_app_kit::{NSEvent, NSView, NSWindow};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = win.window_handle() else {
        return false;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return false;
    };
    let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
    let Some(window) = view.window() else {
        return false;
    };
    let Some(mtm) = objc2_foundation::MainThreadMarker::new() else {
        return false;
    };
    let front =
        NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(NSEvent::mouseLocation(), 0, mtm);
    if front == window.windowNumber() {
        return true;
    }
    unsafe {
        // CGWindowListCreate returns raw CGWindowID values, not CFNumber objects.
        // OnScreenAboveWindow is 1 << 1 and does not require titles or image access.
        let above = CGWindowListCreate(1 << 1, window.windowNumber() as u32);
        if above.is_null() {
            return false;
        }
        let covered = (0..CFArrayGetCount(above))
            .any(|i| CFArrayGetValueAtIndex(above, i) as usize == front as usize);
        CFRelease(above);
        !covered
    }
}

/// NSCursor::set is ignored for inactive applications unless their WindowServer
/// connection opts in. Activating the pet instead would steal the user's typing.
pub(crate) fn enable_background_cursor() -> Result<(), &'static str> {
    type MainConnection = unsafe extern "C" fn() -> i32;
    type SetProperty = unsafe extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32;

    // These are private macOS symbols: resolve them at runtime so a future OS can
    // remove them without preventing the pet from starting or accepting clicks.
    unsafe {
        let connection = libc::dlsym(libc::RTLD_DEFAULT, c"CGSMainConnectionID".as_ptr());
        let set_property = libc::dlsym(libc::RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr());
        if connection.is_null() || set_property.is_null() {
            return Err("WindowServer cursor support is missing");
        }
        let connection: MainConnection = std::mem::transmute(connection);
        let set_property: SetProperty = std::mem::transmute(set_property);
        let cid = connection();
        if cid == 0 {
            return Err("WindowServer connection is unavailable");
        }
        // NSString/NSNumber are toll-free bridged to CFString/CFBoolean. Keep both
        // retained across the call because WindowServer reads their values here.
        let key = NSString::from_str("SetsCursorInBackground");
        let enabled = NSNumber::numberWithBool(true);
        let result = set_property(
            cid,
            cid,
            (&*key as *const NSString).cast(),
            (&*enabled as *const NSNumber).cast(),
        );
        if result != 0 {
            return Err("WindowServer rejected background cursor updates");
        }
    }
    Ok(())
}
