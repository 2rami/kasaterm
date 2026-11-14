/// `claude agents --json --all` 한 벌을 앱 안 여럿이 나눠 쓴다. 부를 때마다 node 를 띄우고
/// 키체인(`security`)을 여는 일이라 한 번에 CPU 280ms·메모리 150MB 남짓이다 — 배경 에이전트
/// 폴러(3초)와 명부 캐시(5초)가 따로 띄우던 시절엔 초당 0.5번이었다(2026-10-06 실측).
/// `max_age` 보다 새것이 있으면 그것을, 아니면 지금 읽어 채운다. 동시에 부르면 하나만 띄우고
/// 나머지는 그 결과를 기다린다. 읽기에 실패하면 `None`(빈 목록과 다르다).
pub fn claude_agents_all(max_age: std::time::Duration) -> Option<std::sync::Arc<Vec<serde_json::Value>>> {
    type Shared = Option<(std::time::Instant, std::sync::Arc<Vec<serde_json::Value>>)>;
    static LAST: std::sync::Mutex<Shared> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, list)) = last.as_ref() {
        if at.elapsed() < max_age {
            return Some(list.clone());
        }
    }
    // 세션 끝 중엔 띄워도 데스크톱이 내려가 실패한다(kasa_socket::session_end) — 낡았어도 가진 것을 준다.
    if kasa_socket::session_end::is_ending() {
        return last.as_ref().map(|(_, list)| list.clone());
    }
    let out = kasa_socket::process::no_window_command(claude_bin()).args(["agents", "--json", "--all"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok()?;
    let list = value
        .as_array()
        .cloned()
        .or_else(|| value.get("agents").and_then(|a| a.as_array().cloned()))?;
    let list = std::sync::Arc::new(list);
    *last = Some((std::time::Instant::now(), list.clone()));
    Some(list)
}

/// Locate the `claude` binary. A GUI app's PATH is minimal (launchd, not the
/// login shell), so PATH lookup alone misses npm-global/local installs — probe
/// the common locations, honoring `CLAUDE_BIN` for an explicit override, and
/// fall back to bare `claude` (PATH) as a last resort.
pub fn claude_bin() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_BIN") {
        if !p.is_empty() {
            return p.into();
        }
    }
    let home = kasa_socket::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let candidates = [
        format!("{home}/.claude/local/claude"),
        format!("{home}/.npm-global/bin/claude"),
        format!("{home}/.local/bin/claude"),
        "/opt/homebrew/bin/claude".to_string(),
        "/usr/local/bin/claude".to_string(),
    ];
    for c in candidates {
        if std::path::Path::new(&c).exists() {
            return c.into();
        }
    }
    "claude".into()
}
