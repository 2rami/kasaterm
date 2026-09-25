//! 예전 kasaspace MCP 등록을 걷어 낸다. 호스트는 한때 부팅할 때마다 AI 클라이언트
//! 설정(`~/.claude.json`, Antigravity)에 `kasaspace` 항목을 써 넣었다. 도구를
//! `kasaterm-cli` 로 옮긴 뒤에도 그 항목은 기기마다 남아서, 사람이 손으로 지워도 다음
//! 부팅이 되살렸고 남은 항목은 사라진 `/mcp` 를 가리킨다. 모든 기기가 새 판으로
//! 한 번씩 부팅하고 나면 이 모듈은 할 일이 없다.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// 우리가 썼던 모양(`127.0.0.1:<포트>/mcp`)일 때만 지운다 — 같은 이름을 사람이
/// 다른 서버에 붙여 뒀을 수도 있다. 파일에 없으면 건드리지 않는다.
pub fn unregister_clients() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    remove_kasaspace(&home.join(".claude.json"), "url");
    remove_kasaspace(&home.join(".gemini/antigravity/mcp_config.json"), "serverUrl");
}

fn remove_kasaspace(path: &Path, url_key: &str) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(mut root) = serde_json::from_str::<Value>(&text) else { return };
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else { return };
    let ours = servers
        .get("kasaspace")
        .and_then(|e| e.get(url_key))
        .and_then(Value::as_str)
        .is_some_and(|u| u.starts_with("http://127.0.0.1:") && u.ends_with("/mcp"));
    if !ours {
        return;
    }
    servers.remove("kasaspace");
    match serde_json::to_string_pretty(&root) {
        Ok(s) => match std::fs::write(path, s) {
            Ok(()) => eprintln!("[kasaspace-mcp] removed stale kasaspace entry from {path:?}"),
            Err(e) => eprintln!("[kasaspace-mcp] unregister {path:?} failed: {e}"),
        },
        Err(e) => eprintln!("[kasaspace-mcp] serialize for {path:?} failed: {e}"),
    }
}
