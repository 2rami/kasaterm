use std::ffi::c_void;

use objc2_foundation::{NSNumber, NSString};

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
