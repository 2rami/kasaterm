//! 살아 있는 claude 세션 명부(`~/.claude/sessions/<pid>.json`) 읽기.
//!
//! claude 는 cross-session 메시징을 위해 자기 자신을 여기 등록한다. 우리가 이걸
//! 읽는 이유는 하나다 — **어떤 pane 에 말을 걸 수 있는지 추측하지 않기 위해서.**
//!
//! `SendMessage` 의 성공 응답은 도달 증명이 아니다(이미 죽은 상대에게 보내도
//! "Message sent" 가 온다). 그래서 보내기 전에 명부를 봐야 하는데, 지금까지는
//! 사람이 `ListAgents` 를 눈으로 보고 이름을 짐작했다. 2026-08-10 새벽에 실제로
//! 그렇게 어긋났다: 같은 캐릭터 pane 이 둘이라 엉뚱한 쪽에 브리프를 보냈고,
//! 정작 상대는 명부에 없어 애초에 닿지도 않았다.
//!
//! ⚠️ **이름으로 잇지 마라.** 명부의 `name` 은 `/rename` 으로 바뀌고(예: pane 이
//! `arisu-p116-ybz` 인데 명부엔 `agy code`), 같은 캐릭터가 여러 pane 에 뜨면
//! 겹치기도 한다. 안정적인 열쇠는 **`sessionId`** 뿐이다 — kasaterm 은 pane 마다
//! 그 값을 이미 쥐고 있다(`pane_claude_sid`).

use std::collections::HashMap;
use std::path::PathBuf;

/// 명부에 등록된 세션 하나.
#[derive(Debug, Clone)]
pub struct Peer {
    /// 안정적인 신원. pane↔명부를 잇는 유일한 열쇠다.
    pub session_id: String,
    /// `SendMessage` 의 `to` 에 그대로 넣을 이름. `/rename` 을 따라 바뀐다.
    pub name: String,
    pub pid: u32,
    /// cross-session 소켓. 이 파일이 없으면 등록만 남고 길은 끊긴 것이다.
    pub socket_path: PathBuf,
}

/// pane 에 말을 걸 수 있는 방법. 모르면 모른다고 말하는 것이 이 타입의 요점이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// 명부에 있고 소켓도 살아 있다 — `SendMessage` 로 닿는다.
    Message,
    /// 명부에 없다. pane 은 도는데 등록이 안 됐거나(비-claude·다른 하네스)
    /// 등록을 거부당한 세션이다 — `kasaterm-cli tell` 로만 닿는다.
    Tell,
    /// 명부에는 있는데 소켓이나 프로세스가 없다. **닿지 않는데 목록에는 보이는**
    /// 가장 위험한 상태라 따로 이름을 준다 — "있으니 닿겠지"를 막는 자리다.
    Stale,
}

impl Reach {
    pub fn as_str(self) -> &'static str {
        match self {
            Reach::Message => "message",
            Reach::Tell => "tell",
            Reach::Stale => "stale",
        }
    }
}

fn registry_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/sessions"))
}

/// 명부를 통째로 읽는다. 파일 수십 개 규모라 캐시 없이 매번 읽어도 싸고,
/// 캐시를 두면 "방금 뜬 pane 이 안 보인다"는 더 나쁜 문제가 생긴다.
pub fn read_registry() -> Vec<Peer> {
    let Some(dir) = registry_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let text = std::fs::read_to_string(&path).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            let session_id = v.get("sessionId")?.as_str()?.to_string();
            if session_id.is_empty() {
                return None;
            }
            // pid 는 문자열로 적힌다(파일명과 같은 값). 숫자로 오는 경우도 받아 준다.
            let pid = v
                .get("pid")
                .and_then(|p| p.as_str().and_then(|s| s.parse().ok()).or_else(|| p.as_u64().map(|n| n as u32)))
                .unwrap_or(0);
            Some(Peer {
                session_id,
                name: v.get("name").and_then(|n| n.as_str()).unwrap_or_default().to_string(),
                pid,
                socket_path: v
                    .get("messagingSocketPath")
                    .and_then(|s| s.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// `sessionId → Peer`. pane 쪽이 쥔 세션 id 로 바로 찾으라고 만든 색인이다.
pub fn by_session_id() -> HashMap<String, Peer> {
    read_registry().into_iter().map(|p| (p.session_id.clone(), p)).collect()
}

/// 이 pane 에 어떻게 말을 걸 수 있나.
///
/// `peer` 는 명부 조회 결과(없으면 None), `pid_alive` 는 호출부가 이미 들고 있는
/// 프로세스 표로 판정한 값이다 — 여기서 `ps` 를 또 부르지 않으려는 것이다.
/// 소켓 파일만 보고 살아 있다고 하면 안 된다: 프로세스가 죽어도 파일은 남는다.
pub fn reach_of(peer: Option<&Peer>, pid_alive: bool) -> Reach {
    let Some(p) = peer else { return Reach::Tell };
    let socket_ok = !p.socket_path.as_os_str().is_empty() && p.socket_path.exists();
    if socket_ok && pid_alive { Reach::Message } else { Reach::Stale }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(sock: &str) -> Peer {
        Peer {
            session_id: "sid".into(),
            name: "aru-p1-abc".into(),
            pid: 42,
            socket_path: PathBuf::from(sock),
        }
    }

    #[test]
    fn no_registry_entry_means_tell_only() {
        // 명부에 없다고 죽은 게 아니다 — codex pane 이나 등록을 못 한 세션이다.
        assert_eq!(reach_of(None, true), Reach::Tell);
    }

    #[test]
    fn live_socket_and_process_is_reachable() {
        let dir = std::env::temp_dir().join("kasaterm-peers-test");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("live.sock");
        std::fs::write(&sock, b"").unwrap();
        assert_eq!(reach_of(Some(&peer(sock.to_str().unwrap())), true), Reach::Message);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn dead_process_with_leftover_socket_is_stale() {
        // 프로세스가 죽어도 소켓 파일은 남는다. 파일만 보고 "닿는다"고 하면
        // 목록에는 보이는데 메시지는 사라지는, 제일 헷갈리는 상태가 된다.
        let dir = std::env::temp_dir().join("kasaterm-peers-test");
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("orphan.sock");
        std::fs::write(&sock, b"").unwrap();
        assert_eq!(reach_of(Some(&peer(sock.to_str().unwrap())), false), Reach::Stale);
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn registered_without_socket_is_stale() {
        assert_eq!(reach_of(Some(&peer("")), true), Reach::Stale);
        assert_eq!(reach_of(Some(&peer("/nonexistent/x.sock")), true), Reach::Stale);
    }
}
