//! 화면 서비스(`kasaterm-screen`)와 잇는 앱 쪽 — 명부 기기·주인 폰을 넘기고, 서비스 카사넷 노드 주소를 받아 `/version` 의
//! `kasanet.screen_node` 로 싣는다(`docs/screen-service-split.md` 「앱 ↔ 서비스 제어 길」). 새 보는 쪽은 그 노드로 붙어 이 앱이 다시
//! 떠도 화면이 이어진다. 서비스가 없거나 규약 판이 다르면 싣지 않고, 보는 쪽은 지금처럼 이 앱으로 붙는다.
//!
//! `KASATERM_SCREEN_SERVICE=off` 면 서비스를 안 쓴다. 서비스 설정은 설정 파일의 `screen_service`
//! (`{input, audio, source_mute, logon_screen}`, 없는 칸은 켬)를 그대로 넘긴다.

/// 부팅 때 한 번 — 카사넷 노드가 선 뒤.
pub(crate) fn spawn() {
    if std::env::var("KASATERM_SCREEN_SERVICE").is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false")) {
        return;
    }
    #[cfg(windows)]
    let _ = std::thread::Builder::new().name("screen-service-client".into()).spawn(win::run);
}

#[cfg(windows)]
mod win {
    use std::time::Duration;

    use kasa_cast::service::{ask, PROTO};
    use serde_json::{json, Value};

    const RETRY: Duration = Duration::from_secs(5);
    const EVERY: Duration = Duration::from_secs(2);

    pub(super) fn run() {
        let mut said_missing = false;
        loop {
            match kasa_cast_host::service::connect_ctl() {
                Some(mut pipe) => {
                    said_missing = false;
                    let why = session(&mut pipe);
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

    fn session(pipe: &mut std::fs::File) -> String {
        let hello = match ask(pipe, &json!({ "t": "hello", "proto": PROTO, "app": env!("CARGO_PKG_VERSION") })) {
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
                if let Err(e) = ask(pipe, &body) {
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
                if let Err(e) = ask(pipe, &body) {
                    return format!("allow: {e}");
                }
                sent = Some(viewers);
            }
            match ask(pipe, &json!({ "t": "status" })) {
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
}
