/// `Command` with the console window suppressed on Windows. kasaterm is a GUI
/// (non-console) process, so spawning a console program (git, etc.) flashes a
/// fresh console window each call — and a polled spawn flashes it on a loop.
/// CREATE_NO_WINDOW keeps it hidden. No-op on other platforms.
pub fn no_window_command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    // `mut` 는 아래 windows 블록이 쓴다 — 떼면 그쪽 빌드가 깨지므로
    // 다른 플랫폼에서만 나는 경고를 끈다.
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}
