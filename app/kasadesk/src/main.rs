//! KasaDesk — 다른 기기 화면을 보는 따로 띄우는 앱(`docs/screen-service-split.md` 「보기 앱」). 본 앱(카사텀)과 따로 돌아 본 앱이
//! 꺼지거나 다시 떠도 화면이 이어진다. 자기 카사넷 키로 그 기기 화면 서비스(없으면 앱)에 바로 붙는다 — 그 기기는 이 기기 본 앱이
//! `/version` 에 알린 보기 앱 id(`view_id`)를 배워 화면만 내준다. 명부는 이 기기 본 앱에서 배우고 상태 폴더에 남긴다.
//!
//! `kasadesk [--machine <기기>] [--app <본 앱 base>]` — 이미 떠 있으면 그쪽에 넘기고 끝난다.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod desk;
mod ipc;
mod net;
mod rig;
mod roster;
mod scene;

use std::path::PathBuf;
use std::time::Duration;

use winit::event_loop::EventLoop;

/// 명부를 다시 묻는 간격.
const ROSTER_EVERY: Duration = Duration::from_secs(5);
/// 기록이 이보다 크면 띄울 때 비운다.
const LOG_MAX: u64 = 2 << 20;

/// 상태 폴더 — 하나만 띄우기 소켓·지난 명부·기록. 격리 리그는 `KASATERM_VIEW_STATE`.
fn state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("KASATERM_VIEW_STATE").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    #[cfg(target_os = "macos")]
    return Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/kasaterm/desk"));
    #[cfg(windows)]
    return Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("kasaterm").join("desk"));
    #[cfg(not(any(target_os = "macos", windows)))]
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".local/state/kasaterm/desk"))
}

/// Finder·시작 메뉴로 띄우면 stderr 가 어디에도 안 남는다 — 상태 폴더 기록으로 돌린다.
fn log_to(path: &std::path::Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::remove_file(path);
    }
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: 열린 파일의 fd 를 stderr 자리로 복제한다 — 파일은 프로세스가 끝날 때까지 살려 둔다.
        unsafe { libc::dup2(file.as_raw_fd(), 2) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: 열린 파일 핸들을 표준 오류로 건다 — 파일은 프로세스가 끝날 때까지 살려 둔다.
        unsafe { windows_sys::Win32::System::Console::SetStdHandle(windows_sys::Win32::System::Console::STD_ERROR_HANDLE, file.as_raw_handle()) };
    }
    std::mem::forget(file);
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() -> anyhow::Result<()> {
    let state = state_dir().ok_or_else(|| anyhow::anyhow!("상태 폴더를 모른다"))?;
    std::fs::create_dir_all(&state)?;
    let open = arg("--machine");
    let app = arg("--app")
        .or_else(|| std::env::var("KASATERM_VIEW_APP").ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "http://127.0.0.1:8765".into());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("kasadesk-net").enable_all().build()?;
    if rt.block_on(ipc::hand_over(&state, open.as_deref())) {
        return Ok(());
    }
    if std::env::var_os("KASATERM_VIEW_STDERR").is_none() {
        log_to(&state.join("kasadesk.log"));
    }
    eprintln!("[kasadesk] {} 시작 — 본 앱 {app}", env!("CARGO_PKG_VERSION"));
    let event_loop = EventLoop::<desk::Ev>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    {
        let _rt = rt.enter();
        let proxy = proxy.clone();
        if let Err(e) = ipc::listen(&state, move |label| drop(proxy.send_event(desk::Ev::Open(label)))) {
            eprintln!("[kasadesk] 하나만 띄우기 길을 못 연다: {e}");
        }
    }
    let net = net::Net::start(rt.handle().clone())?;
    {
        let (net, proxy, cache) = (net.clone(), proxy.clone(), state.join("roster.json"));
        std::thread::Builder::new().name("kasadesk-roster".into()).spawn(move || {
            let mut known = roster::load(&cache);
            loop {
                let snap = roster::merge(roster::fetch(&app), &known);
                net.follow(&snap.machines);
                if snap.app_alive && snap.machines != known {
                    roster::save(&cache, &snap.machines);
                }
                known = snap.machines.clone();
                if proxy.send_event(desk::Ev::Roster { machines: snap.machines, app_alive: snap.app_alive }).is_err() {
                    return;
                }
                std::thread::sleep(ROSTER_EVERY);
            }
        })?;
    }
    let mut desk = desk::Desk::new(proxy, net, open);
    event_loop.run_app(&mut desk)?;
    Ok(())
}
