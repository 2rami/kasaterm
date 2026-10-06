//! 호스트가 꽂는 것. 보드 수집기와 tell 장부는 기기 명부·관문·원격 칸을 직접 모른다 —
//! 본판은 kasa-mcp 의 명부·관문·원격 칸으로, `kasa tui` 는 자기 것으로 채운다.
//! 꽂지 않으면 명부 없음·관문 없음·호스트 이름 표시의 홀로 선 기계다.

use std::sync::{Arc, RwLock};

/// 판을 당겨 올 다른 기계 하나.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Route {
    /// 사람이 보는 이름.
    pub label: String,
    /// 그 기계의 영구 id. 모르면 첫 폴링에서 배운다.
    pub machine_id: Option<String>,
    /// `/collab/*` 가 사는 HTTP 주소.
    pub base: String,
}

pub trait CollabEnv: Send + Sync {
    /// 판에 보일 이 기계 이름.
    fn self_label(&self) -> String;
    /// 판을 당겨 올 다른 기계들.
    fn machines(&self) -> Vec<Route> {
        Vec::new()
    }
    /// 관문 우회 길의 뿌리(관문 주소, 주인 slug). 관문이 없으면 `None`.
    fn relay(&self) -> Option<(String, String)> {
        None
    }
    /// 그 주소로 갈 때 실을 토큰.
    fn auth_token(&self, _base: &str) -> Option<String> {
        None
    }
    /// 다른 기계 칸을 비추는 거울 칸이면 참 — 그 칸에는 전역 주소로만 보낸다.
    fn is_remote_pane(&self, _surface: &str) -> bool {
        false
    }
}

struct Standalone;

impl CollabEnv for Standalone {
    fn self_label(&self) -> String {
        machine_label()
    }
}

static ENV: RwLock<Option<Arc<dyn CollabEnv>>> = RwLock::new(None);

/// 호스트가 켤 때 한 번 꽂는다. 다시 꽂으면 바꾼다.
pub fn set_env(env: Arc<dyn CollabEnv>) {
    *ENV.write().unwrap_or_else(|e| e.into_inner()) = Some(env);
}

pub fn env() -> Arc<dyn CollabEnv> {
    ENV.read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(Standalone))
}

/// 이 기계 이름 — 판·`tell 이름@기계` 에 보이는 것. 본판과 `kasa tui` 가 같은 이름을 낸다.
/// `KASATERM_SELF_LABEL` 이 앞서고, macOS 는 컴퓨터 이름(scutil), 윈도우는 `COMPUTERNAME`,
/// 그 밖은 `hostname -s` 다. 윈도우엔 scutil 이 없고 hostname.exe 는 `-s` 를 거부한다 — 그 둘만
/// 보면 윈도우 기기가 다른 기기 판에 「이 기계」로 떴다(2026-09-27).
pub fn machine_label() -> String {
    if let Ok(v) = std::env::var("KASATERM_SELF_LABEL") {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    let run = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    run("scutil", &["--get", "ComputerName"])
        .or_else(|| std::env::var("COMPUTERNAME").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .or_else(|| run("hostname", &["-s"]))
        .unwrap_or_else(|| "이 기계".to_string())
}
