//! 화면 서비스(`kasaterm-screen`)와 잇는 앱 쪽 — 명부 기기·주인 폰을 넘기고, 서비스 카사넷 노드 주소를 받아 `/version` 의
//! `kasanet.screen_node` 로 싣는다(`docs/screen-service-split.md` 「앱 ↔ 서비스 제어 길」). 새 보는 쪽은 그 노드로 붙어 이 앱이 다시
//! 떠도 화면이 이어진다. 서비스가 없거나 규약 판이 다르면 싣지 않고, 보는 쪽은 지금처럼 이 앱으로 붙는다.
//!
//! 길: Windows 는 서비스의 이름 파이프, macOS 는 상태 폴더의 유닉스 소켓. macOS 는 앱이 번들 안 도우미를 `SMAppService` 로 등록한다.
//! `KASATERM_SCREEN_SERVICE=off` 면 서비스를 안 쓴다. 서비스 설정은 설정 파일의 `screen_service`
//! (`{enabled, input, audio, source_mute, logon_screen}`, 없는 칸은 켬)를 넘긴다 — `enabled: false` 면 맥 도우미 등록을 푼다.

use std::io::{Read, Write};
use std::time::Duration;

use kasa_cast::service::{ask, PROTO};
use serde_json::{json, Value};

const RETRY: Duration = Duration::from_secs(5);
const EVERY: Duration = Duration::from_secs(2);

/// 부팅 때 한 번 — 카사넷 노드가 선 뒤.
pub(crate) fn spawn() {
    if std::env::var("KASATERM_SCREEN_SERVICE").is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false")) {
        return;
    }
    let enabled = screen_settings().and_then(|s| s.get("enabled").and_then(Value::as_bool)).unwrap_or(true);
    #[cfg(target_os = "macos")]
    mac::register(enabled);
    if !enabled {
        return;
    }
    #[cfg(any(windows, target_os = "macos"))]
    let _ = std::thread::Builder::new().name("screen-service-client".into()).spawn(run);
}

#[cfg(any(windows, target_os = "macos"))]
fn run() {
    let mut said_missing = false;
    loop {
        #[cfg(windows)]
        let stream = kasa_cast_host::service::connect_ctl();
        #[cfg(target_os = "macos")]
        let stream = mac::connect();
        match stream {
            Some(mut stream) => {
                said_missing = false;
                let why = session(&mut stream);
                kasa_device::kasanet::set_screen_node(None);
                eprintln!("[screen-service] 화면 서비스와 끊김: {why}");
            }
            None if !said_missing => {
                said_missing = true;
                eprintln!("[screen-service] 화면 서비스가 없다 — 보는 쪽은 이 앱으로 붙는다");
            }
            None => {}
        }
        std::thread::sleep(RETRY);
    }
}

fn session(io: &mut (impl Read + Write)) -> String {
    let hello = match ask(io, &json!({ "t": "hello", "proto": PROTO, "app": env!("CARGO_PKG_VERSION") })) {
        Ok(v) => v,
        Err(e) => return format!("hello: {e}"),
    };
    if hello.get("proto").and_then(Value::as_u64) != Some(u64::from(PROTO)) {
        // 판이 다른 서비스와는 말을 섞지 않는다 — 업데이트가 맞춰 줄 때까지 이 앱이 보낸다.
        std::thread::sleep(Duration::from_secs(60));
        return format!("규약 판이 다르다: {}", hello.get("proto").cloned().unwrap_or(Value::Null));
    }
    eprintln!(
        "[screen-service] 화면 서비스 {} 붙음 — 노드 {}",
        hello.get("version").and_then(Value::as_str).unwrap_or("?"),
        hello.pointer("/node/id").and_then(Value::as_str).map_or("?", |id| &id[..id.len().min(10)])
    );
    let mut sent = None;
    let mut sent_settings = None;
    loop {
        let settings = screen_settings();
        if settings.is_some() && sent_settings != settings {
            let mut body = settings.clone().unwrap_or_default();
            body["t"] = json!("settings");
            if let Err(e) = ask(io, &body) {
                return format!("settings: {e}");
            }
            sent_settings = settings;
        }
        let viewers = kasa_device::kasanet::screen_viewers();
        if sent.as_ref() != Some(&viewers) {
            let (machines, phones) = &viewers;
            let body = json!({
                "t": "allow",
                "machines": machines,
                "phones": phones.iter().map(|(id, until)| json!([id, until])).collect::<Vec<_>>(),
            });
            if let Err(e) = ask(io, &body) {
                return format!("allow: {e}");
            }
            sent = Some(viewers);
        }
        match ask(io, &json!({ "t": "status" })) {
            Ok(status) => kasa_device::kasanet::set_screen_node(status.get("node").cloned().filter(|n| n.get("id").is_some())),
            Err(e) => return format!("status: {e}"),
        }
        std::thread::sleep(EVERY);
    }
}

/// 설정 파일의 `screen_service` 칸 — 없으면 None(서비스가 쥔 값 그대로).
fn screen_settings() -> Option<Value> {
    let path = crate::socket::settings_file_path()?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("screen_service").filter(|s| s.is_object()).cloned()
}

#[cfg(target_os = "macos")]
mod mac {
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    use objc2_foundation::ns_string;
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    /// 앱 번들 안 LaunchAgent 정의(`Contents/Library/LaunchAgents/`) — `scripts/build-app.sh` 가 넣는다.
    const PLIST: &str = "com.kasa.kasaterm.screen.plist";

    /// 번들 안 도우미를 로그인 항목(백그라운드)으로 등록한다 — 등록하면 launchd 가 바로 띄우고 로그인마다 띄운다. 검증 실행·번들
    /// 밖(개발 실행)·macOS 13 이전은 건드리지 않는다(앱 안에서 보낸다).
    pub(super) fn register(enabled: bool) {
        if crate::verification_run() {
            return;
        }
        let in_bundle = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent()?.parent().map(|c| c.join("Library/LaunchAgents").join(PLIST)))
            .is_some_and(|p| p.is_file());
        if !in_bundle || objc2::runtime::AnyClass::get(c"SMAppService").is_none() {
            return;
        }
        let service = unsafe { SMAppService::agentServiceWithPlistName(ns_string!("com.kasa.kasaterm.screen.plist")) };
        let status = unsafe { service.status() };
        if !enabled {
            if status == SMAppServiceStatus::Enabled || status == SMAppServiceStatus::RequiresApproval {
                if let Err(e) = unsafe { service.unregisterAndReturnError() } {
                    eprintln!("[screen-service] 도우미 등록을 못 풀었다: {e}");
                }
            }
            return;
        }
        if status == SMAppServiceStatus::Enabled {
            return;
        }
        if status == SMAppServiceStatus::RequiresApproval {
            eprintln!("[screen-service] 화면 도우미가 「로그인 항목」에서 허용을 기다린다 — 시스템 설정 › 일반 › 로그인 항목");
            return;
        }
        match unsafe { service.registerAndReturnError() } {
            Ok(()) => eprintln!("[screen-service] 화면 도우미를 등록했다"),
            Err(e) => eprintln!("[screen-service] 화면 도우미를 등록 못 했다: {e}"),
        }
    }

    /// 상태 폴더의 제어 소켓 — 도우미와 같은 자리(`KASATERM_SCREEN_STATE` 가 있으면 거기).
    pub(super) fn connect() -> Option<UnixStream> {
        let dir = match std::env::var_os("KASATERM_SCREEN_STATE").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support/kasaterm/screen"),
        };
        let stream = UnixStream::connect(dir.join("ctl.sock")).ok()?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok()?;
        Some(stream)
    }
}
