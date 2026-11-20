//! Windows 화면 도우미 — UAC·잠금 화면(보안 데스크톱)까지 보고 누르기(`docs/screen-stream.md` S3b). Parsec 과 같은 꼴이다.
//!
//! - 서비스 `KasatermScreen`(LocalSystem, 설치기가 건다, `kasaterm.exe --screen-service`)이 지금 콘솔 세션에 도우미
//!   (`kasaterm.exe --screen-agent`)를 **SYSTEM 으로** 띄우고, 세션이 바뀌거나 도우미가 죽으면 다시 띄운다. 서비스가 멎으면
//!   잡 오브젝트가 도우미도 거둔다.
//! - 도우미는 뜨기·압축(`win::start_local`)·조작을 맡는다. SYSTEM 이라야 입력 데스크톱이 Winlogon 으로 바뀔 때 그리로 스레드를
//!   옮겨 복제·SendInput 을 할 수 있다.
//! - 사용자 앱은 세션 파이프 `\\.\pipe\kasaterm-screen-<세션>` 으로 붙는다. 파이프는 SYSTEM 과 그 세션 사용자만 열 수 있다.
//!   도우미는 붙은 프로세스가 같은 kasaterm.exe 인지, 앱은 파이프 주인이 SYSTEM 인지 본다(사용자 프로세스는 SYSTEM
//!   프로세스의 경로를 못 읽고, 이름을 먼저 차지한 사용자 프로그램은 주인을 SYSTEM 으로 못 정한다).
//!
//! 틀: `[종류 u8][길이 u32 LE][본문]`. 장 연결은 앱이 `S`(설정 JSON)를 보내면 도우미가 `N`(정보 JSON, `id` 포함)·`D`(거절)
//! 를 돌려주고 그 뒤로 `F`(장)·`H`(1초 숨) 만 보낸다. 조작 연결은 앱이 `C` 뒤로 `K`(세션 id u64 LE, 키프레임)·`I`(조작 JSON)
//! 만 보낸다. 한 연결은 한 방향으로만 흐른다 — 동기 파이프 핸들은 읽기가 막혀 있으면 같은 핸들의 쓰기도 막힌다.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use kasa_cast::mirror::{Denied, Input};
use kasa_cast::{Config, Frame, Session};
use serde_json::{json, Value};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_CALL_NOT_IMPLEMENTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE, NO_ERROR, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{
    DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId, TokenUser,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_ALL_ACCESS, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, WaitNamedPipeW,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceConfigW, RegisterServiceCtrlHandlerExW,
    SetServiceStatus, StartServiceCtrlDispatcherW, QUERY_SERVICE_CONFIGW, SC_ACTION, SC_ACTION_RESTART, SC_MANAGER_CONNECT,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CHANGE_CONFIG, SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_FAILURE_ACTIONSW, SERVICE_FAILURE_ACTIONS_FLAG,
    SERVICE_QUERY_CONFIG, SERVICE_RUNNING, SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, ResumeThread,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
};

pub(crate) const SERVICE_NAME: &str = "KasatermScreen";

/// 서비스 이름 — 시험 서비스 리그는 `KASATERM_SCREEN_SERVICE_NAME` 으로 갈라 사람 서비스의 설정(복구 동작·등록 경로)을 안 건드린다.
fn service_name() -> String {
    std::env::var("KASATERM_SCREEN_SERVICE_NAME")
        .ok()
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| SERVICE_NAME.to_string())
}
/// 장 연결에 장이 없을 때도 이만큼마다 숨을 보내 — 앱이 떠난 것을 도우미가 쓰기 실패로 알고, 앱 쪽 읽기 스레드도 멈춤 표시를
/// 볼 수 있다.
const BREATH: Duration = Duration::from_secs(1);
const MAX_BODY: usize = 64 << 20;

/// 서비스로 돌 때 도우미 감독과 함께 돌릴 일 — 화면 서비스 실행 파일(`kasaterm-screen`)이 카사넷 화면 노드·제어 파이프를
/// 여기서 연다(`docs/screen-service-split.md`). 받은 깃발이 서면 멈춘다.
static SERVICE_WORK: std::sync::OnceLock<fn(&'static AtomicBool)> = std::sync::OnceLock::new();

/// [`run`] 과 같고, 서비스로 돌면 `work` 를 따로 스레드에서 함께 돌린다.
pub fn run_with(args: &[OsString], work: fn(&'static AtomicBool)) -> bool {
    let _ = SERVICE_WORK.set(work);
    run(args)
}

/// 화면 서비스 상태 폴더 — 서비스 키·허용 목록·설정·스피커 표. SYSTEM·Administrators 만 열게 둔다.
/// 시험 서비스 리그는 `KASATERM_SCREEN_STATE` 로 따로 둔다.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KASATERM_SCREEN_STATE").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("ProgramData")?).join("kasaterm").join("screen"))
}

/// `kasaterm.exe`·`kasaterm-screen.exe` 가 서비스·도우미로 불렸으면 그 일을 하고 참. 앱의 로그·설정·세션 무엇 하나 건드리기 전에 부른다.
pub(crate) fn run(args: &[OsString]) -> bool {
    match args.first().and_then(|a| a.to_str()) {
        Some("--screen-service") => {
            service_dispatch();
            true
        }
        Some("--screen-agent") => {
            agent_main();
            true
        }
        _ => false,
    }
}

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// 도우미 파이프 이름. 시험 서비스 리그는 `KASATERM_SCREEN_PIPE` 로 이름을 갈라 사람 서비스의 도우미와 섞이지 않는다.
fn pipe_name(session: u32) -> String {
    let prefix = std::env::var("KASATERM_SCREEN_PIPE").ok().filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    format!(r"\\.\pipe\{}-{session}", prefix.as_deref().unwrap_or("kasaterm-screen"))
}

fn my_session() -> Option<u32> {
    let mut session = 0;
    unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.ok()?;
    Some(session)
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

/// 파이프 건너편이 믿는 실행 파일인가 — 이 도우미와 같은 실행 파일(화면 서비스, 옛 꼴이면 앱 자신)이거나 같은 설치의 앱
/// (`bin\screen\<판>\kasaterm-screen.exe` 옆 `bin\kasaterm.exe`). 설치판은 관리자만 쓰는 Program Files 에 있다.
fn trusted_peer(pid: u32) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();
    let (Some(peer), Ok(me)) = (image_of(pid), std::env::current_exe()) else { return false };
    norm(&peer) == norm(&me)
        || me.parent().and_then(Path::parent).and_then(Path::parent).is_some_and(|bin| norm(&peer) == norm(&bin.join("kasaterm.exe")))
}

fn sid_text(sid: PSID) -> Option<String> {
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.ok()?;
    let s = unsafe { text.to_string() }.ok();
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    s
}

/// 이 파이프를 SYSTEM 이 만들었나 — 도우미는 주인을 SYSTEM 으로 박아 만든다.
fn owned_by_system(pipe: &File) -> bool {
    let (mut owner, mut descriptor) = (PSID::default(), PSECURITY_DESCRIPTOR::default());
    let got = unsafe {
        GetSecurityInfo(
            HANDLE(pipe.as_raw_handle()),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    let system = got == NO_ERROR && sid_text(owner).as_deref() == Some("S-1-5-18");
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    system
}

fn send(w: &mut impl Write, kind: u8, body: &[u8]) -> std::io::Result<()> {
    let mut msg = Vec::with_capacity(body.len() + 5);
    msg.push(kind);
    msg.extend_from_slice(&(body.len() as u32).to_le_bytes());
    msg.extend_from_slice(body);
    w.write_all(&msg)
}

fn recv(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_BODY {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

fn input_json(display: u32, input: &Input) -> Value {
    match input {
        Input::Pointer { x, y, buttons, prev, mods } => {
            json!({ "d": display, "t": "pointer", "x": x, "y": y, "b": buttons, "p": prev, "m": mods })
        }
        Input::Scroll { x, y, dx, dy } => json!({ "d": display, "t": "scroll", "x": x, "y": y, "dx": dx, "dy": dy }),
        Input::Key { code, down, mods } => json!({ "d": display, "t": "key", "code": code, "down": down, "m": mods }),
        Input::Text(text) => json!({ "d": display, "t": "text", "s": text }),
    }
}

fn input_from(v: &Value) -> Option<(u32, Input)> {
    let display = u32::try_from(v.get("d")?.as_u64()?).ok()?;
    let unit = |k: &str| v.get(k).and_then(Value::as_f64).filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0));
    let small = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0).min(255) as u8;
    let wheel = |k: &str| v.get(k).and_then(Value::as_i64).unwrap_or(0).clamp(-10_000, 10_000) as i32;
    let input = match v.get("t")?.as_str()? {
        "pointer" => Input::Pointer { x: unit("x")?, y: unit("y")?, buttons: small("b") & 7, prev: small("p") & 7, mods: small("m") & 15 },
        "scroll" => Input::Scroll { x: unit("x")?, y: unit("y")?, dx: wheel("dx"), dy: wheel("dy") },
        "key" => Input::Key {
            code: v.get("code")?.as_str()?.chars().take(32).collect(),
            down: v.get("down").and_then(Value::as_bool),
            mods: small("m") & 15,
        },
        "text" => Input::Text(v.get("s")?.as_str()?.chars().take(4096).collect()),
        _ => return None,
    };
    Some((display, input))
}

fn denied_of(reason: &[u8]) -> Denied {
    if reason == b"permission" {
        Denied::Permission
    } else {
        Denied::Unavailable
    }
}

// ---- 앱 쪽 ----

/// 이 세션의 도우미에 붙는다 — 세션 0 의 화면 서비스는 지금 콘솔 세션의 도우미에. 없으면(서비스 없는 설치) None — 앱이 직접 뜬다.
pub(super) fn connect() -> Option<File> {
    let session = match my_session()? {
        0 => unsafe { WTSGetActiveConsoleSessionId() },
        s => s,
    };
    let name = pipe_name(session);
    for _ in 0..3 {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            // 같은 이름을 먼저 차지한 다른 프로그램에 화면·조작을 넘기지 않는다.
            Ok(file) => return owned_by_system(&file).then_some(file),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                let wide_name = wide(&name);
                let _ = unsafe { WaitNamedPipeW(PCWSTR(wide_name.as_ptr()), 500) };
            }
            Err(_) => return None,
        }
    }
    None
}

/// 화면 서비스 제어 파이프에 붙는다(앱 쪽). 주인이 SYSTEM 인 파이프만 — 같은 이름을 먼저 차지한 사용자 프로그램에 명부를
/// 넘기지 않는다.
pub fn connect_ctl() -> Option<File> {
    let name = format!(r"\\.\pipe\{}", kasa_cast::service::ctl_name());
    let file = std::fs::OpenOptions::new().read(true).write(true).open(&name).ok()?;
    owned_by_system(&file).then_some(file)
}

struct AgentSession {
    id: u64,
    info: Value,
    display: u32,
    stop: Arc<AtomicBool>,
}

impl Session for AgentSession {
    fn keyframe(&self) {
        control(b'K', &self.id.to_le_bytes());
    }

    fn display(&self) -> u32 {
        self.display
    }

    fn info(&self) -> Value {
        self.info.clone()
    }

    fn set_bitrate(&self, bps: u32) {
        let mut body = self.id.to_le_bytes().to_vec();
        body.extend_from_slice(&bps.to_le_bytes());
        control(b'R', &body);
    }
}

impl Drop for AgentSession {
    /// 읽기 스레드가 다음 장·숨에서 멈추고 파이프를 닫는다 — 도우미는 쓰기 실패로 세션을 거둔다.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

pub(super) fn start(mut pipe: File, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>, display: u32) -> Result<Box<dyn Session>, Denied> {
    send(&mut pipe, b'S', config.to_json().to_string().as_bytes()).map_err(|_| Denied::Unavailable)?;
    let (kind, body) = recv(&mut pipe).map_err(|_| Denied::Unavailable)?;
    match kind {
        b'N' => {
            let mut info: Value = serde_json::from_slice(&body).map_err(|_| Denied::Unavailable)?;
            let id = info.get("id").and_then(Value::as_u64).ok_or(Denied::Unavailable)?;
            if let Some(o) = info.as_object_mut() {
                o.remove("id");
                o.insert("agent".into(), json!(true));
            }
            let stop = Arc::new(AtomicBool::new(false));
            let reader_stop = stop.clone();
            std::thread::Builder::new()
                .name("screen-agent-frames".into())
                .spawn(move || {
                    while !reader_stop.load(Ordering::Relaxed) {
                        match recv(&mut pipe) {
                            Ok((b'F', body)) => {
                                if let Some(frame) = Frame::decode(&body) {
                                    out(frame);
                                }
                            }
                            Ok(_) => {}
                            Err(_) => return,
                        }
                    }
                })
                .map_err(|_| Denied::Unavailable)?;
            Ok(Box::new(AgentSession { id, info, display, stop }))
        }
        b'D' => Err(denied_of(&body)),
        _ => Err(Denied::Unavailable),
    }
}

/// 조작·키프레임 청은 앱에 하나인 조작 연결로 — 끊겼으면 한 번 다시 붙는다. 도우미가 없으면 거짓.
fn control(kind: u8, body: &[u8]) -> bool {
    static CONTROL: Mutex<Option<File>> = Mutex::new(None);
    let mut conn = CONTROL.lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..2 {
        if conn.is_none() {
            *conn = connect().and_then(|mut f| send(&mut f, b'C', &[]).ok().map(|()| f));
        }
        let Some(file) = conn.as_mut() else { return false };
        if send(file, kind, body).is_ok() {
            return true;
        }
        *conn = None;
    }
    false
}

/// 도우미가 조작을 받았으면 참 — 보안 데스크톱에도 들어간다.
pub(super) fn input(display: u32, input: &Input) -> bool {
    control(b'I', input_json(display, input).to_string().as_bytes())
}

// ---- 도우미(SYSTEM, 사용자 세션) ----

type Sessions = Arc<Mutex<HashMap<u64, Box<dyn Session>>>>;

fn agent_main() {
    redirect_stderr();
    super::win::com();
    let Some(session) = my_session() else { return };
    if let Some(dir) = state_dir() {
        crate::set_state_dir(dir);
    }
    crate::source_mute::restore_after_crash();
    // 로그인 전(로그인 화면)에는 세션 사용자가 없다 — 화면 서비스(SYSTEM)만 붙게 열고, 사용자가 생기면 끝나 서비스가 그
    // 사용자까지 받게 다시 띄운다.
    let sddl = match session_user_sid(session) {
        Some(sid) => wide(format!("O:SYD:P(A;;GA;;;SY)(A;;GRGW;;;{sid})")),
        None => {
            let _ = std::thread::Builder::new().name("screen-agent-logon".into()).spawn(move || loop {
                std::thread::sleep(Duration::from_secs(2));
                if session_user_sid(session).is_some() {
                    log("세션 사용자가 생겼다 — 그 사용자도 받게 다시 뜬다");
                    std::process::exit(0);
                }
            });
            wide("O:SYD:P(A;;GA;;;SY)")
        }
    };
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(PCWSTR(sddl.as_ptr()), SDDL_REVISION_1, &mut descriptor, None) }.is_err() {
        log("파이프 보안 설명자를 못 만든다");
        return;
    }
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<(u32, Input)>();
    let _ = std::thread::Builder::new().name("screen-agent-input".into()).spawn(move || {
        let mut desk = None;
        for (display, input) in input_rx {
            super::win::follow_input_desktop(&mut desk);
            let _ = crate::input::put(display, &input);
        }
    });
    let name = wide(pipe_name(session));
    log(&format!("도우미 시작 — 세션 {session}"));
    let mut first = true;
    loop {
        let flags = if first { PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE } else { PIPE_ACCESS_DUPLEX };
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                flags,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                1 << 20,
                1 << 16,
                0,
                Some(&attrs),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            if first {
                // 다른 프로그램이 이 이름을 먼저 쥐었다 — 그쪽과 섞이지 않게 물러난다(서비스가 잠시 뒤 다시 띄운다).
                log("파이프 이름을 이미 누가 쥐었다");
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        first = false;
        // 앱이 ConnectNamedPipe 보다 먼저 붙으면 ERROR_PIPE_CONNECTED 로 온다 — 그것도 연결이다.
        let connected = match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => true,
            Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
        };
        let file = unsafe { File::from_raw_handle(pipe.0) };
        let mut pid = 0;
        let trusted = connected && unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) }.is_ok() && trusted_peer(pid);
        if !trusted {
            continue;
        }
        let (sessions, input_tx) = (sessions.clone(), input_tx.clone());
        let _ = std::thread::Builder::new().name("screen-agent-conn".into()).spawn(move || serve(file, &sessions, &input_tx));
    }
}

/// 지금 콘솔 세션에 로그인한 사용자가 있나 — 없으면 로그인 화면이다.
pub fn console_has_user() -> bool {
    session_user_sid(unsafe { WTSGetActiveConsoleSessionId() }).is_some()
}

fn session_user_sid(session: u32) -> Option<String> {
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }.ok()?;
    let mut len = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    // TOKEN_USER 의 포인터를 읽으니 8바이트 정렬 버퍼로.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    let got = unsafe { GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len) };
    let _ = unsafe { CloseHandle(token) };
    got.ok()?;
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    sid_text(user.User.Sid)
}

fn serve(mut pipe: File, sessions: &Sessions, input_tx: &mpsc::Sender<(u32, Input)>) {
    let Ok((kind, body)) = recv(&mut pipe) else { return };
    match kind {
        b'S' => serve_frames(pipe, &body, sessions),
        b'A' => serve_audio(pipe, body.first().is_none_or(|m| *m != 0)),
        b'C' => {
            while let Ok((kind, body)) = recv(&mut pipe) {
                match kind {
                    b'K' if body.len() == 8 => {
                        let id = u64::from_le_bytes(body.try_into().unwrap_or([0; 8]));
                        if let Some(s) = sessions.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
                            s.keyframe();
                        }
                    }
                    b'I' => {
                        if let Some(x) = serde_json::from_slice::<Value>(&body).ok().as_ref().and_then(input_from) {
                            let _ = input_tx.send(x);
                        }
                    }
                    b'R' if body.len() == 12 => {
                        let id = u64::from_le_bytes(body[..8].try_into().unwrap_or([0; 8]));
                        let bps = u32::from_le_bytes(body[8..].try_into().unwrap_or([0; 4]));
                        if let Some(s) = sessions.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
                            s.set_bitrate(bps);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn serve_frames(mut pipe: File, body: &[u8], sessions: &Sessions) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let config = serde_json::from_slice::<Value>(body).map(|v| Config::from_json(&v)).unwrap_or_default();
    let (tx, rx) = mpsc::channel::<Frame>();
    let out: Box<dyn Fn(Frame) + Send + Sync> = Box::new(move |f| {
        let _ = tx.send(f);
    });
    let session = match super::win::start_local(&config, out, super::win::chosen_display(config.display)) {
        Ok(s) => s,
        Err(d) => {
            let reason: &[u8] = if d == Denied::Permission { b"permission" } else { b"unavailable" };
            let _ = send(&mut pipe, b'D', reason);
            return;
        }
    };
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut info = session.info();
    info["id"] = json!(id);
    if send(&mut pipe, b'N', info.to_string().as_bytes()).is_err() {
        return;
    }
    sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(id, session);
    loop {
        let sent = match rx.recv_timeout(BREATH) {
            Ok(frame) => send(&mut pipe, b'F', &frame.encode()),
            Err(mpsc::RecvTimeoutError::Timeout) => send(&mut pipe, b'H', &[]),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if sent.is_err() {
            break;
        }
    }
    // 세션을 놓는 동안(스레드 거두기) 표를 쥐지 않는다.
    let gone = sessions.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    drop(gone);
}

/// 소리 연결 — 기본 출력 장치 루프백을 떠 `a`(QPC 초 f64 LE + s16le 48kHz 스테레오) 덩이로 보낸다. 뜨는 동안 이 기기
/// 스피커를 끈다(`source_mute`). 소리가 없으면 루프백이 꾸러미를 안 줘 1초마다 숨(`H`)을 보낸다 — 떠난 것을 쓰기 실패로 안다.
fn serve_audio(mut pipe: File, mute: bool) {
    let (tx, rx) = mpsc::channel::<(f64, Vec<i16>)>();
    let started = kasa_cast::audio::Source::start(&crate::audio_win::Source, Box::new(move |t, pcm: &[i16]| drop(tx.send((t, pcm.to_vec())))));
    let running = match started {
        Ok(r) => r,
        Err(e) => {
            let _ = send(&mut pipe, b'D', e.as_bytes());
            return;
        }
    };
    crate::set_source_mute(mute);
    let muted = crate::source_mute::mute();
    if send(&mut pipe, b'N', &[]).is_err() {
        return;
    }
    loop {
        let sent = match rx.recv_timeout(BREATH) {
            Ok((t, pcm)) => {
                let mut body = Vec::with_capacity(8 + pcm.len() * 2);
                body.extend_from_slice(&t.to_le_bytes());
                for s in pcm {
                    body.extend_from_slice(&s.to_le_bytes());
                }
                send(&mut pipe, b'a', &body)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => send(&mut pipe, b'H', &[]),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if sent.is_err() {
            break;
        }
    }
    drop(muted);
    drop(running);
}

/// 도우미가 뜨는 시스템 소리 — 세션 0 의 화면 서비스가 쓴다(세션 0 에는 소리 장치 세션이 없다).
pub(crate) struct AgentAudio;

struct AgentAudioRunning(Arc<AtomicBool>);

impl Drop for AgentAudioRunning {
    /// 읽기 스레드가 다음 덩이·숨에서 멈추고 파이프를 닫는다 — 도우미는 쓰기 실패로 뜨기를 멈추고 스피커를 되켠다.
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl kasa_cast::audio::Source for AgentAudio {
    fn start(&self, mut out: Box<dyn FnMut(f64, &[i16]) + Send>) -> Result<Box<dyn Send>, String> {
        let mut pipe = connect().ok_or("화면 도우미가 없다")?;
        send(&mut pipe, b'A', &[u8::from(crate::source_mute_on())]).map_err(|e| e.to_string())?;
        match recv(&mut pipe) {
            Ok((b'N', _)) => {}
            Ok((b'D', why)) => return Err(String::from_utf8_lossy(&why).into_owned()),
            _ => return Err("화면 도우미 소리 연결이 끊겼다".into()),
        }
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        std::thread::Builder::new()
            .name("screen-agent-audio".into())
            .spawn(move || {
                let mut pcm = Vec::new();
                while !s.load(Ordering::Relaxed) {
                    match recv(&mut pipe) {
                        Ok((b'a', body)) if body.len() >= 8 => {
                            let t = f64::from_le_bytes(body[..8].try_into().unwrap_or([0; 8]));
                            pcm.clear();
                            pcm.extend(body[8..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])));
                            out(t, &pcm);
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Box::new(AgentAudioRunning(stop)))
    }
}

/// SYSTEM 프로세스라 표준 오류가 어디에도 안 남는다 — 화면 스트림의 `eprintln!` 까지 `%ProgramData%\kasaterm\screen-service.log`
/// 로 보낸다(Rust 의 표준 오류는 쓸 때마다 표준 핸들을 다시 읽는다).
fn redirect_stderr() {
    use std::os::windows::io::IntoRawHandle;
    // 상태 폴더 옆(`%ProgramData%\kasaterm`) — 시험 서비스 리그는 제 상태 폴더 옆에 따로 남는다.
    let Some(dir) = state_dir().and_then(|d| d.parent().map(Path::to_path_buf)) else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("screen-service.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = unsafe { windows::Win32::System::Console::SetStdHandle(windows::Win32::System::Console::STD_ERROR_HANDLE, HANDLE(f.into_raw_handle())) };
    }
}

fn log(line: &str) {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    eprintln!("{stamp} [{}] {line}", std::process::id());
}

// ---- 서비스(SYSTEM, 세션 0) ----

static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicUsize = AtomicUsize::new(0);
/// 끝날 때 「실패」로 알려 서비스 관리자가 다시 띄우게 — 판이 바뀐 실행 파일로 갈아탈 때.
static RESTART: AtomicBool = AtomicBool::new(false);
/// 서비스 고유 실패 — 복구 동작(다시 시작)이 이것을 보고 다시 띄운다.
const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

fn set_state(state: SERVICE_STATUS_CURRENT_STATE, accept: u32) {
    set_state_exit(state, accept, 0);
}

fn set_state_exit(state: SERVICE_STATUS_CURRENT_STATE, accept: u32, specific: u32) {
    let handle = SERVICE_STATUS_HANDLE(STATUS.load(Ordering::Acquire) as *mut _);
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accept,
        dwWin32ExitCode: if specific == 0 { 0 } else { ERROR_SERVICE_SPECIFIC_ERROR },
        dwServiceSpecificExitCode: specific,
        dwCheckPoint: 0,
        dwWaitHint: 5000,
    };
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

/// 서비스를 끝내고 서비스 관리자가 다시 띄우게 한다 — MSI 가 새 판 실행 파일을 깔고 등록 경로만 바꿔 두면(돌던 서비스는
/// 안 멈춘다), 보는 사람이 없을 때 이것으로 갈아탄다(`docs/screen-service-split.md` 「Windows — MSI」).
pub fn restart_service() {
    RESTART.store(true, Ordering::Relaxed);
    STOP.store(true, Ordering::Relaxed);
}

/// 서비스 관리자에 적힌 이 서비스의 실행 파일(등록 경로의 첫 토막).
pub fn configured_exe() -> Option<PathBuf> {
    let name = wide(service_name());
    let manager = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }.ok()?;
    let service = unsafe { OpenServiceW(manager, PCWSTR(name.as_ptr()), SERVICE_QUERY_CONFIG) };
    let out = service.ok().and_then(|service| {
        let mut need = 0u32;
        let _ = unsafe { QueryServiceConfigW(service, None, 0, &mut need) };
        let mut buf = vec![0u64; (need as usize).div_ceil(8).max(1)];
        let got = unsafe { QueryServiceConfigW(service, Some(buf.as_mut_ptr().cast()), need, &mut need) };
        let _ = unsafe { CloseServiceHandle(service) };
        got.ok()?;
        let config = unsafe { &*(buf.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
        let line = unsafe { config.lpBinaryPathName.to_string() }.ok()?;
        Some(PathBuf::from(exe_of_command(&line)))
    });
    let _ = unsafe { CloseServiceHandle(manager) };
    out
}

/// `"C:\a b\x.exe" --flag` → `C:\a b\x.exe`.
fn exe_of_command(line: &str) -> String {
    let line = line.trim();
    match line.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().to_string(),
        None => line.split_whitespace().next().unwrap_or_default().to_string(),
    }
}

/// 죽거나 갈아타려고 끝나면 서비스 관리자가 다시 띄우게 — 「실패로 끝남」에도 복구 동작을 건다.
fn configure_recovery() {
    let name = wide(service_name());
    let Ok(manager) = (unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }) else { return };
    if let Ok(service) = unsafe { OpenServiceW(manager, PCWSTR(name.as_ptr()), SERVICE_CHANGE_CONFIG | SERVICE_START) } {
        let mut actions = [
            SC_ACTION { Type: SC_ACTION_RESTART, Delay: 1000 },
            SC_ACTION { Type: SC_ACTION_RESTART, Delay: 5000 },
            SC_ACTION { Type: SC_ACTION_RESTART, Delay: 30_000 },
        ];
        let failure = SERVICE_FAILURE_ACTIONSW {
            dwResetPeriod: 24 * 3600,
            lpRebootMsg: PWSTR::null(),
            lpCommand: PWSTR::null(),
            cActions: actions.len() as u32,
            lpsaActions: actions.as_mut_ptr(),
        };
        let flag = SERVICE_FAILURE_ACTIONS_FLAG { fFailureActionsOnNonCrashFailures: true.into() };
        unsafe {
            let _ = ChangeServiceConfig2W(service, SERVICE_CONFIG_FAILURE_ACTIONS, Some((&failure as *const SERVICE_FAILURE_ACTIONSW).cast()));
            let _ = ChangeServiceConfig2W(service, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, Some((&flag as *const SERVICE_FAILURE_ACTIONS_FLAG).cast()));
            let _ = CloseServiceHandle(service);
        }
    }
    let _ = unsafe { CloseServiceHandle(manager) };
}

fn service_dispatch() {
    let mut name = wide(service_name());
    let table = [
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(name.as_mut_ptr()), lpServiceProc: Some(service_main) },
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR::null(), lpServiceProc: None },
    ];
    // 서비스 관리자 밖에서(손으로) 부르면 곧바로 실패하고 끝난다.
    let _ = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = wide(service_name());
    let Ok(handle) = (unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(service_control), None) }) else { return };
    STATUS.store(handle.0 as usize, Ordering::Release);
    set_state(SERVICE_RUNNING, SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN);
    redirect_stderr();
    log("서비스 시작");
    configure_recovery();
    if let Some(work) = SERVICE_WORK.get().copied() {
        let _ = std::thread::Builder::new().name("screen-service-work".into()).spawn(move || work(&STOP));
    }
    supervise();
    if RESTART.load(Ordering::Relaxed) {
        log("서비스 멈춤 — 새 판으로 다시 뜬다");
        set_state_exit(SERVICE_STOPPED, 0, 1);
    } else {
        log("서비스 멈춤");
        set_state(SERVICE_STOPPED, 0);
    }
}

unsafe extern "system" fn service_control(control: u32, _event: u32, _data: *mut std::ffi::c_void, _context: *mut std::ffi::c_void) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::Relaxed);
            set_state(SERVICE_STOP_PENDING, 0);
            NO_ERROR.0
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

/// 지금 콘솔 세션에 도우미 하나를 살려 둔다. 거듭 죽으면 1초부터 30초까지 물러서며 다시 띄운다.
fn supervise() {
    let job = kill_on_close_job();
    let mut child: Option<(HANDLE, u32, Instant)> = None;
    let (mut backoff, mut next_try) = (Duration::from_secs(1), Instant::now());
    while !STOP.load(Ordering::Relaxed) {
        let active = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some((process, session, started)) = child {
            let dead = unsafe { WaitForSingleObject(process, 0) } != WAIT_TIMEOUT;
            if dead || session != active {
                if !dead {
                    let _ = unsafe { TerminateProcess(process, 0) };
                }
                let _ = unsafe { CloseHandle(process) };
                child = None;
                if dead {
                    if started.elapsed() > Duration::from_secs(60) {
                        backoff = Duration::from_secs(1);
                    }
                    next_try = Instant::now() + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
        if child.is_none() && active != u32::MAX && Instant::now() >= next_try {
            match launch(active, job) {
                Ok(process) => child = Some((process, active, Instant::now())),
                Err(e) => {
                    log(&format!("도우미를 못 띄운다(세션 {active}): {e}"));
                    next_try = Instant::now() + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some((process, ..)) = child {
        let _ = unsafe { TerminateProcess(process, 0) };
        let _ = unsafe { CloseHandle(process) };
    }
    if let Some(job) = job {
        let _ = unsafe { CloseHandle(job) };
    }
}

/// 서비스가 어떻게 끝나든(죽어도) 도우미가 남지 않게.
fn kill_on_close_job() -> Option<HANDLE> {
    let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?;
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let set = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    set.is_ok().then_some(job)
}

/// 이 서비스의 SYSTEM 토큰을 복제해 세션 번호만 바꿔 그 세션의 기본 데스크톱에 도우미를 띄운다.
fn launch(session: u32, job: Option<HANDLE>) -> windows::core::Result<HANDLE> {
    let mut own = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own)? };
    let mut token = HANDLE::default();
    let dup = unsafe { DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token) };
    let _ = unsafe { CloseHandle(own) };
    dup?;
    let started = (|| {
        unsafe { SetTokenInformation(token, TokenSessionId, (&session as *const u32).cast(), 4)? };
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        let exe_w = wide(&exe);
        let mut command = wide(format!("\"{}\" --screen-agent", exe.display()));
        let mut desktop = wide("winsta0\\default");
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessAsUserW(
                Some(token),
                PCWSTR(exe_w.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                None,
                PCWSTR::null(),
                &startup,
                &mut info,
            )?
        };
        if let Some(job) = job {
            let _ = unsafe { AssignProcessToJobObject(job, info.hProcess) };
        }
        unsafe { ResumeThread(info.hThread) };
        let _ = unsafe { CloseHandle(info.hThread) };
        Ok(info.hProcess)
    })();
    let _ = unsafe { CloseHandle(token) };
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_round_trip_through_the_pipe_json() {
        let all = [
            Input::Pointer { x: 0.25, y: 1.0, buttons: 1, prev: 0, mods: 9 },
            Input::Scroll { x: 0.5, y: 0.5, dx: -3, dy: 120 },
            Input::Key { code: "KeyA".into(), down: Some(true), mods: 2 },
            Input::Key { code: "Enter".into(), down: None, mods: 0 },
            Input::Text("한글 abc".into()),
        ];
        for input in all {
            assert_eq!(input_from(&input_json(3, &input)), Some((3, input)));
        }
        assert_eq!(input_from(&json!({ "d": 0, "t": "pointer", "x": 9.0, "y": -1.0 })).map(|x| x.1), Some(Input::Pointer { x: 1.0, y: 0.0, buttons: 0, prev: 0, mods: 0 }));
        assert_eq!(input_from(&json!({ "t": "text", "s": "x" })), None, "화면 번호 없는 조작은 버린다");
    }

    #[test]
    fn service_command_line_gives_the_exe() {
        assert_eq!(exe_of_command(r#""C:\Program Files\kasaterm\bin\screen\1.0.0\kasaterm-screen.exe" --screen-service"#), r"C:\Program Files\kasaterm\bin\screen\1.0.0\kasaterm-screen.exe");
        assert_eq!(exe_of_command(r"C:\x\a.exe --screen-service"), r"C:\x\a.exe");
    }

    #[test]
    fn messages_frame_with_kind_and_length() {
        let mut buf = Vec::new();
        send(&mut buf, b'K', &7u64.to_le_bytes()).unwrap();
        send(&mut buf, b'H', &[]).unwrap();
        let mut r = buf.as_slice();
        assert_eq!(recv(&mut r).unwrap(), (b'K', 7u64.to_le_bytes().to_vec()));
        assert_eq!(recv(&mut r).unwrap(), (b'H', Vec::new()));
        assert!(recv(&mut r).is_err());
    }
}
