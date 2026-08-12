//! Client config injection. MCP has no auto-discovery: each AI client
//! (Claude Code, Antigravity, …) only connects to servers listed in its
//! own config file. So when the kasaterm host boots its HTTP MCP server,
//! it writes a `kasaspace` entry into each known client's config pointing
//! at the real bound port. Idempotent — only the kasaspace entry is
//! touched; everything else in the file is preserved.

use std::path::PathBuf;

use serde_json::{json, Value};

/// Inject `kasaspace` into every known local AI client's MCP config so any
/// agent launched on this machine auto-discovers our tools. Best-effort:
/// a missing/locked file for one client never blocks the others.
///
/// **정본 포트를 잡은 인스턴스만 쓴다.** 이 등록은 클라이언트당 슬롯이 하나뿐인
/// 전역 파일이라, 두 번째 인스턴스가 쓰면 첫 번째를 밀어낸다. `spawn_http_server`
/// 는 정본 포트가 이미 물려 있으면 임시 포트(bind 0)로 떨어지므로 `port !=
/// canonical_port` 가 곧 "이 주소의 주인은 내가 아니다" 다.
///
/// 안 막았을 때 실제로 난 일(2026-08-05): pane 에서 `cargo run -p kasaterm` 한 번에
/// `~/.claude.json` 이 그 dev 인스턴스의 임시 포트로 바뀌고, 인스턴스가 죽으면 **죽은
/// 포트가 남아** 진짜 앱 pane 의 claude 들이 kasaspace 도구를 통째로 잃었다. 소켓과
/// 포트 파일은 pid 로 갈라 같은 사고를 이미 막아 뒀는데 이 전역 등록만 안 갈려 있었다
/// — 짝이 되는 표면은 같은 기준으로 함께 갈라야 한다.
pub fn register_clients(port: u16, canonical_port: u16) {
    if port != canonical_port {
        eprintln!(
            "[kasaspace-mcp] port {port} != canonical {canonical_port} — 전역 MCP 등록 생략 \
             (주인 인스턴스의 주소를 덮지 않는다)"
        );
        return;
    }
    let url = format!("http://127.0.0.1:{port}/mcp");
    register_claude(&url);
    register_antigravity(&url);
    // codex (~/.codex/config.toml) speaks TOML and its HTTP MCP transport
    // landed in later releases; skipped until we gate on the version.
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Read a JSON file into a Value (empty object if missing/garbage), set
/// `mcpServers.kasaspace = entry`, and write it back pretty-printed. Using
/// serde_json::Value keeps every other key intact.
fn upsert_mcp_server(path: &PathBuf, entry: Value, create_if_absent: bool) {
    let exists = path.exists();
    if !exists && !create_if_absent {
        return;
    }
    let mut root: Value = if exists {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
            Err(_) => return,
        }
    } else {
        json!({})
    };
    if !root.is_object() {
        root = json!({});
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        *servers = json!({});
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert("kasaspace".to_string(), entry);

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(&root) {
        Ok(s) => {
            if let Err(e) = std::fs::write(path, s) {
                eprintln!("[kasaspace-mcp] register {path:?} failed: {e}");
            } else {
                eprintln!("[kasaspace-mcp] registered kasaspace in {path:?}");
            }
        }
        Err(e) => eprintln!("[kasaspace-mcp] serialize for {path:?} failed: {e}"),
    }
}

/// Claude Code: `~/.claude.json`, HTTP transport. Only touch it if it
/// already exists (it always does after Claude's first run) so we don't
/// fabricate a config Claude would otherwise own.
fn register_claude(url: &str) {
    let Some(h) = home() else { return };
    let path = h.join(".claude.json");
    upsert_mcp_server(&path, json!({ "type": "http", "url": url }), false);
}

/// Antigravity: `~/.gemini/antigravity/mcp_config.json`, `serverUrl` key.
/// Small dedicated file, safe to create if absent.
fn register_antigravity(url: &str) {
    let Some(h) = home() else { return };
    let path = h.join(".gemini/antigravity/mcp_config.json");
    upsert_mcp_server(&path, json!({ "serverUrl": url }), true);
}
