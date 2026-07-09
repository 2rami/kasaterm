//! Spawning external programs from kasaterm's GUI process. On Windows a GUI
//! (non-console) process flashes a fresh console window every time it spawns a
//! console program (git, claude, python). A polled spawn flashes it on a loop —
//! the "검은창 자꾸 생겼다가 사라져" symptom. CREATE_NO_WINDOW suppresses it.
//! Route every external spawn through here so no site reintroduces the flash.
//! No-op on other platforms.

use std::ffi::OsStr;
use std::process::Command;

pub(crate) fn command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}
