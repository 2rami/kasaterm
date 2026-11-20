//! Windows 화면 서비스의 일 — 세션 0 의 `KasatermScreen` 서비스 안에서 도우미 감독과 나란히 돈다.
//! 카사넷 화면 노드(장·소리는 콘솔 세션 도우미 파이프에서), 앱이 붙는 제어 파이프, 상태 폴더 잠그기.

use std::ffi::OsStr;
use std::fs::File;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kasa_cast::Capture;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_PIPE_CONNECTED, HLOCAL, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

use crate::ctl::{self, Service};
use crate::node::{self, Node};
use crate::state::{unix_now, State};

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

fn ctl_pipe_name() -> String {
    format!(r"\\.\pipe\{}", kasa_cast::service::ctl_name())
}

/// 서비스 스레드에서 — 멈춤 깃발이 설 때까지.
pub fn serve(stop: &'static AtomicBool) {
    let Some(dir) = kasa_cast_host::service::state_dir() else {
        eprintln!("[screen-service] ProgramData 를 모른다 — 화면 노드를 안 연다");
        return;
    };
    let state = State::new(dir.clone());
    lock_down(&dir);
    drop_old_versions();
    kasa_cast_host::set_state_dir(dir);
    let settings = Arc::new(Mutex::new(state.settings()));
    ctl::apply(*settings.lock().unwrap_or_else(|e| e.into_inner()));
    let Some(inner) = kasa_cast_host::capture() else { return };
    let capture: Arc<dyn Capture> = Arc::new(ctl::Gate { inner, settings: settings.clone() });
    kasa_cast::audio::install(kasa_cast_host::service::audio());
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[screen-service] 런타임을 못 띄웠다: {e}");
            return;
        }
    };
    let key = match node::load_key(&state.key_path()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("[screen-service] 서비스 키를 못 읽어 끈다: {e}");
            return;
        }
    };
    let allowed = state.allowed();
    let started = rt.block_on(Node::start(key, capture, kasa_cast::audio::hub(), settings.clone(), &allowed.live(unix_now())));
    let node = match started {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[screen-service] 화면 노드를 못 띄웠다: {e:#}");
            return;
        }
    };
    let _rt = rt.enter();
    let service = Arc::new(Service::new(state, node, settings, allowed));
    {
        let service = service.clone();
        let _ = std::thread::Builder::new().name("screen-service-ctl".into()).spawn(move || listen(&service));
    }
    while !stop.load(Ordering::Relaxed) {
        service.tick();
        // MSI 는 새 판을 옆 폴더에 깔고 등록 경로만 바꾼다(돌던 서비스는 안 멈춘다). 보는 사람이 다 떠났을 때 갈아탄다.
        if service.node.viewers.count() == 0 && moved_on() {
            eprintln!("[screen-service] 새 판이 깔렸다 — 보는 사람이 없어 갈아탄다");
            kasa_cast_host::service::restart_service();
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    rt.block_on(service.node.shutdown());
    service.state.write_public_status(crate::VERSION, 0, &serde_json::Value::Null);
    eprintln!("[screen-service] 화면 노드 닫음");
}

/// 서비스 관리자에 적힌 실행 파일이 이 프로세스와 다른가 — 새 판이 깔렸다.
fn moved_on() -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();
    match (kasa_cast_host::service::configured_exe(), std::env::current_exe()) {
        (Some(configured), Ok(me)) => norm(&configured) != norm(&me) && configured.is_file(),
        _ => false,
    }
}

/// 지난 판 폴더를 치운다 — 서비스 구성요소는 업데이트가 안 지우게(Permanent) 깔려 `bin\screen\<판>\` 이 쌓인다. 지금 판
/// 폴더(이 실행 파일)와 등록 경로의 폴더는 둔다.
fn drop_old_versions() {
    let Ok(me) = std::env::current_exe() else { return };
    let Some(mine) = me.parent() else { return };
    let Some(screen) = mine.parent().filter(|d| d.file_name().is_some_and(|n| n.eq_ignore_ascii_case("screen"))) else { return };
    let configured = kasa_cast_host::service::configured_exe().and_then(|p| p.parent().map(Path::to_path_buf));
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();
    let Ok(entries) = std::fs::read_dir(screen) else { return };
    for dir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let keep = norm(&dir) == norm(mine) || configured.as_deref().is_some_and(|c| norm(&dir) == norm(c));
        if !keep && std::fs::remove_dir_all(&dir).is_ok() {
            eprintln!("[screen-service] 지난 판 폴더를 치웠다: {}", dir.display());
        }
    }
}

/// 제어 파이프 — SYSTEM 과 대화형 사용자만 열고, 붙은 프로세스가 같은 설치의 앱(`bin\kasaterm.exe`)인지 본다.
fn listen(service: &Arc<Service>) {
    let sddl = wide("D:P(A;;GA;;;SY)(A;;GRGW;;;IU)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }.is_err() {
        eprintln!("[screen-service] 제어 파이프 보안 설명자를 못 만든다");
        return;
    }
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let name = wide(ctl_pipe_name());
    loop {
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                Some(&attrs),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
        };
        let mut pid = 0;
        let trusted = connected && unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) }.is_ok() && trusted_app(pid);
        let file = unsafe { File::from_raw_handle(pipe.0) };
        if !trusted {
            continue;
        }
        let service = service.clone();
        let _ = std::thread::Builder::new().name("screen-service-ctl-conn".into()).spawn(move || {
            let mut file = file;
            while let Ok(req) = kasa_cast::service::recv(&mut file) {
                if kasa_cast::service::send(&mut file, &service.handle(&req)).is_err() {
                    break;
                }
            }
        });
    }
}

fn image_of(pid: u32) -> Option<PathBuf> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let got = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    let _ = unsafe { CloseHandle(process) };
    got.ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// 같은 설치의 앱인가 — 서비스는 `bin\screen\<판>\kasaterm-screen.exe`, 앱은 `bin\kasaterm.exe`(관리자만 쓰는 Program Files).
fn trusted_app(pid: u32) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();
    let (Some(peer), Ok(me)) = (image_of(pid), std::env::current_exe()) else { return false };
    me.parent().and_then(Path::parent).and_then(Path::parent).is_some_and(|bin| norm(&peer) == norm(&bin.join("kasaterm.exe")))
}

/// 상태 폴더를 SYSTEM·Administrators 만 열게 — `%ProgramData%` 아래 새 폴더는 사용자도 만들고 읽을 수 있게 물려받는다.
fn lock_down(dir: &Path) {
    let sddl = wide("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }.is_err() {
        return;
    }
    let (mut present, mut defaulted) = (false.into(), false.into());
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let got = unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) };
    if got.is_ok() {
        let path = wide(dir);
        let done = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR(path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        };
        if done.is_err() {
            eprintln!("[screen-service] 상태 폴더를 못 잠갔다: {done:?}");
        }
    }
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
}
