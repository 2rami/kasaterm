//! macOS 화면 서비스 — 사용자 LaunchAgent 로 돈다(`docs/screen-service-split.md` 「macOS」). 앱 번들 안
//! `Contents/Library/LoginItems/KasatermScreen.app` 이 실행 파일이고, 앱이 `SMAppService.agent` 로 등록한다. launchd 가 띄우므로
//! 책임 프로세스가 이 도우미라 화면 기록·손쉬운 사용 권한이 앱이 아니라 이 번들(`com.kasa.kasaterm.screen`)에 붙는다.
//!
//! 같은 프로세스가 뜨기·압축·소리·조작(`kasa_cast_host`)과 카사넷 화면 노드, 앱이 붙는 유닉스 소켓을 맡는다.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ctl::{self, Service};
use crate::node::{self, Node};
use crate::state::{unix_now, State};

/// 새 판으로 갈아탈 때의 끝 코드 — launchd(`KeepAlive`)가 바로 다시 띄운다.
const EXIT_UPGRADE: i32 = 75;

/// 상태 폴더 — 서비스 키·허용 목록·설정·제어 소켓. 시험 리그는 `KASATERM_SCREEN_STATE` 로 따로 둔다.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KASATERM_SCREEN_STATE").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/kasaterm/screen"))
}

pub fn ctl_path(dir: &Path) -> PathBuf {
    dir.join("ctl.sock")
}

/// `--launch-agent` — launchd 가 부른다. 끝 코드를 돌려준다.
pub fn serve() -> i32 {
    let Some(dir) = state_dir() else {
        eprintln!("[screen-service] HOME 을 모른다");
        return 1;
    };
    let state = State::new(dir.clone());
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    kasa_cast_host::set_state_dir(dir.clone());
    let settings = Arc::new(Mutex::new(state.settings()));
    ctl::apply(*settings.lock().unwrap_or_else(|e| e.into_inner()));
    let Some(inner) = kasa_cast_host::capture() else { return 1 };
    let capture: Arc<dyn kasa_cast::Capture> = Arc::new(ctl::Gate { inner, settings: settings.clone() });
    kasa_cast_host::audio::install();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[screen-service] 런타임을 못 띄웠다: {e}");
            return 1;
        }
    };
    let key = match node::load_key(&state.key_path()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("[screen-service] 서비스 키를 못 읽어 끈다: {e}");
            return 1;
        }
    };
    let allowed = state.allowed();
    let node = match rt.block_on(Node::start(key, capture, kasa_cast::audio::hub(), settings.clone(), &allowed.live(unix_now()))) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[screen-service] 화면 노드를 못 띄웠다: {e:#}");
            return 1;
        }
    };
    let _rt = rt.enter();
    let service = Arc::new(Service::new(state, node, settings, allowed));
    let listener = match bind(&ctl_path(&dir)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[screen-service] 제어 소켓을 못 열었다: {e}");
            return 1;
        }
    };
    {
        let service = service.clone();
        let _ = std::thread::Builder::new().name("screen-service-ctl".into()).spawn(move || listen(&listener, &service, trusted));
    }
    loop {
        service.tick();
        // Sparkle 이 번들째 갈아 끼우면 돌던 이 프로세스는 옛 파일로 계속 돈다. 번들의 서비스 판이 바뀌었고 보는 사람이 없으면
        // 끝나 launchd 가 새 판을 띄운다 — 결정 9.
        if service.node.viewers.count() == 0 && bundle_version().is_some_and(|v| v != crate::VERSION) {
            eprintln!("[screen-service] 번들에 새 판이 깔렸다 — 보는 사람이 없어 갈아탄다");
            rt.block_on(service.node.shutdown());
            return EXIT_UPGRADE;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// 지난 실행이 남긴 소켓 파일을 치우고 연다. 사용자만 읽고 쓴다(폴더도 0700).
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// 한 연결에 한 스레드 — 요청 하나에 답 하나.
pub fn listen(listener: &UnixListener, service: &Arc<Service>, trust: fn(&UnixStream) -> bool) {
    for stream in listener.incoming().flatten() {
        if !trust(&stream) {
            continue;
        }
        let service = service.clone();
        let _ = std::thread::Builder::new().name("screen-service-ctl-conn".into()).spawn(move || {
            let mut stream = stream;
            while let Ok(req) = kasa_cast::service::recv(&mut stream) {
                if kasa_cast::service::send(&mut stream, &service.handle(&req)).is_err() {
                    break;
                }
            }
        });
    }
}

/// 같은 사용자이고, 같은 번들의 앱(`kasaterm.app/Contents/MacOS/kasaterm`)인가. 검증 실행(리그)은 같은 사용자만 본다 — 리그 앱은 번들 밖에 있다.
fn trusted(stream: &UnixStream) -> bool {
    let fd = stream.as_raw_fd();
    let (mut uid, mut gid) = (0, 0);
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 || uid != unsafe { libc::getuid() } {
        return false;
    }
    if kasa_cast_host::verification_run() {
        return true;
    }
    let Some(peer) = peer_pid(fd).and_then(exe_of) else { return false };
    app_exe().is_some_and(|app| peer == app)
}

fn peer_pid(fd: i32) -> Option<i32> {
    const SOL_LOCAL: i32 = 0;
    const LOCAL_PEERPID: i32 = 2;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let got = unsafe { libc::getsockopt(fd, SOL_LOCAL, LOCAL_PEERPID, (&mut pid as *mut libc::pid_t).cast(), &mut len) };
    (got == 0 && pid > 0).then_some(pid)
}

fn exe_of(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| PathBuf::from(String::from_utf8_lossy(&buf[..n as usize]).into_owned()))
}

/// `…/kasaterm.app/Contents/Library/LoginItems/KasatermScreen.app/Contents/MacOS/kasaterm-screen` → `…/kasaterm.app/Contents/MacOS/kasaterm`.
fn app_exe() -> Option<PathBuf> {
    app_exe_of(&std::env::current_exe().ok()?)
}

fn app_exe_of(me: &Path) -> Option<PathBuf> {
    let contents = me.ancestors().nth(6)?;
    (contents.file_name()? == "Contents").then(|| contents.join("MacOS/kasaterm"))
}

/// 이 실행 파일이 든 번들의 서비스 판(`CFBundleShortVersionString`) — 번들 밖(리그)이면 None.
fn bundle_version() -> Option<String> {
    let me = std::env::current_exe().ok()?;
    let plist = std::fs::read_to_string(me.parent()?.parent()?.join("Info.plist")).ok()?;
    plist_string(&plist, "CFBundleShortVersionString")
}

fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Settings;
    use kasa_net::iroh::SecretKey;
    use serde_json::json;

    #[test]
    fn app_is_found_from_the_helper_bundle() {
        let me = Path::new("/Applications/kasaterm.app/Contents/Library/LoginItems/KasatermScreen.app/Contents/MacOS/kasaterm-screen");
        assert_eq!(app_exe_of(me), Some(PathBuf::from("/Applications/kasaterm.app/Contents/MacOS/kasaterm")));
        assert_eq!(app_exe_of(Path::new("/tmp/rig/kasaterm-screen")), None, "번들 밖이면 모른다");
    }

    #[test]
    fn plist_version_is_read() {
        let p = "<dict>\n<key>CFBundleShortVersionString</key>\n\t<string>1.2.3</string>\n<key>X</key><string>y</string></dict>";
        assert_eq!(plist_string(p, "CFBundleShortVersionString").as_deref(), Some("1.2.3"));
        assert_eq!(plist_string(p, "Nope"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_app_talks_to_the_service_over_the_socket() {
        std::env::set_var("KASATERM_KASANET_BIND", "127.0.0.1:0");
        let root = std::env::temp_dir().join(format!("kasa-screen-sock-{}", std::process::id()));
        let state = State::new(root.join("screen"));
        let settings = Arc::new(Mutex::new(Settings::default()));
        struct Never;
        impl kasa_cast::Capture for Never {
            fn start(&self, _: &kasa_cast::Config, _: Box<dyn Fn(kasa_cast::Frame) + Send + Sync>) -> Result<Box<dyn kasa_cast::Session>, kasa_cast::Denied> {
                Err(kasa_cast::Denied::Unavailable)
            }
            fn now(&self) -> f64 {
                0.0
            }
        }
        let node = Node::start(SecretKey::generate(), Arc::new(Never), None, settings.clone(), &[]).await.unwrap();
        let service = Arc::new(Service::new(state, node, settings, Default::default()));
        let sock = ctl_path(&root);
        let listener = bind(&sock).unwrap();
        let s = service.clone();
        std::thread::spawn(move || listen(&listener, &s, |_| true));
        let reply = tokio::task::spawn_blocking(move || {
            let mut c = UnixStream::connect(&sock).unwrap();
            let hello = kasa_cast::service::ask(&mut c, &json!({ "t": "hello", "proto": kasa_cast::service::PROTO })).unwrap();
            let viewer = SecretKey::generate().public().to_string();
            let ok = kasa_cast::service::ask(&mut c, &json!({ "t": "allow", "machines": [viewer], "phones": [] })).unwrap();
            let status = kasa_cast::service::ask(&mut c, &json!({ "t": "status" })).unwrap();
            (hello, ok, status)
        })
        .await
        .unwrap();
        assert_eq!(reply.0["proto"], kasa_cast::service::PROTO);
        assert_eq!(reply.1["t"], "ok");
        assert_eq!(reply.2["node"]["id"], service.node.endpoint.id().to_string());
        assert_eq!(service.state.allowed().machines.len(), 1, "넘긴 명부는 디스크에 남는다");
        let _ = std::fs::remove_dir_all(&root);
    }
}
