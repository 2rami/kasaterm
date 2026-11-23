//! 따로 까는 보기 앱(KasaDesk, github.com/2rami/kasadesk)으로 화면 보기를 연다 — 이 앱이 꺼지거나 다시 떠도 화면이 이어진다
//! (`docs/screen-service-split.md` 「보기 앱」). KasaDesk 가 안 깔린 기기는 화면 탭으로 물러선다.

use std::path::PathBuf;
use std::process::Stdio;

/// 보기 앱 자리 — 맥 `/Applications`·`~/Applications` 의 `KasaDesk.app`, Windows `Program Files\KasaDesk\kasadesk.exe`.
/// `KASATERM_DESK` 가 있으면 그것만 본다. 검증 실행은 `KASATERM_DESK` 없이는 보기 앱을 안 띄운다 — 깔린 보기 앱은 사람 것이라
/// 떠 있으면 그쪽에 넘어가 버린다.
pub(crate) fn desk_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("KASATERM_DESK").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(path)).filter(|p| launchable(p));
    }
    if crate::verification_run() {
        return None;
    }
    installed().into_iter().find(|p| launchable(p))
}

fn launchable(path: &std::path::Path) -> bool {
    if path.extension().is_some_and(|e| e == "app") { path.join("Contents/MacOS/kasadesk").is_file() } else { path.is_file() }
}

fn installed() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let mut out = vec![PathBuf::from("/Applications/KasaDesk.app")];
        out.extend(std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Applications/KasaDesk.app")));
        out
    }
    #[cfg(windows)]
    {
        ["ProgramW6432", "ProgramFiles"].iter().filter_map(std::env::var_os).map(|dir| PathBuf::from(dir).join("KasaDesk").join("kasadesk.exe")).collect()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// 그 기기를 보기 앱으로 연다. 보기 앱이 이미 떠 있으면 새로 띄운 것이 그쪽에 넘기고 끝난다. 보기 앱이 없으면 Err.
pub(crate) fn open_in_desk(label: &str) -> Result<(), String> {
    let path = desk_path().ok_or("보기 앱이 없다")?;
    let port = std::env::var("KASASPACE_MCP_PORT").ok().filter(|p| p.trim().parse::<u16>().is_ok()).unwrap_or_else(|| "8765".into());
    let app = format!("http://127.0.0.1:{}", port.trim());
    let args = ["--machine", label, "--app", &app];
    #[cfg(target_os = "macos")]
    if path.extension().is_some_and(|e| e == "app") && !crate::verification_run() {
        // LaunchServices 로 띄워 이 앱의 자식이 아니게 — 이 앱이 다시 떠도 산다. `-n` 이 없으면 떠 있는 보기 앱이 인자를 못 받는다.
        return crate::proc::command("/usr/bin/open")
            .args(["-n", "-a"])
            .arg(&path)
            .arg("--args")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|e| format!("open: {e}"));
    }
    let exe = if path.extension().is_some_and(|e| e == "app") { path.join("Contents/MacOS/kasadesk") } else { path };
    let spawn = |detach_job: bool| {
        let mut cmd = crate::proc::command(&exe);
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // 이 앱 프로세스 묶음 밖으로 — 묶음째 가는 신호에 같이 죽지 않게.
            cmd.process_group(0);
            let _ = detach_job;
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            // 이 앱이 작업 개체 안에서 돌면(닫히면 다 죽이는 작업) 보기 앱이 같이 죽는다 — 빠져나가게. 허락 안 된 작업이면 그대로.
            // 창 없는 콘솔(`proc::command` 의 CREATE_NO_WINDOW) 대신 콘솔 없이 뗀다 — 보기 앱은 창 앱이라 콘솔이 안 뜬다.
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | if detach_job { CREATE_BREAKAWAY_FROM_JOB } else { 0 });
        }
        cmd.spawn()
    };
    spawn(true).or_else(|_| spawn(false)).map(drop).map_err(|e| format!("{}: {e}", exe.display()))
}
