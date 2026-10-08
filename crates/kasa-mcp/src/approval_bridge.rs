//! mod 브로커(`claude_mod`, docs/claude-mod-bridge.md 「권한 요청」) ↔ 관문 원격 승인(docs/remote-approval.md).
//!
//! 이 기기 학생이 권한을 물으면(브로커에 요청이 열리면) 비밀만 가려 관문에 올리고, 관문에서 결정이 오면 브로커에
//! 돌려준다. 자리에서 엔진 창으로 먼저 답했거나 칸이 사라지면 관문 요청을 닫아 폰·다른 맥 화면을 걷는다.
//! 관문의 2분이 지나면 원격 결정은 더 받지 않는다 — 엔진 창은 처음부터 함께 떠 있으니 그대로 거기서 답한다.
//!
//! 로그인이 없거나 관문에 닿지 못하면 아무것도 올리지 않는다(엔진 창만). 계정 주인 기기 확인은 관문이 한다.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::claude_mod;
use crate::device_auth::approvals as gateway;

/// 브로커 요청(로컬 id) → 관문 요청.
#[derive(Clone)]
struct Up {
    gateway: String,
    digest: String,
    surface: String,
    session: String,
}

/// 앱 프로세스에서 한 번 — 자기 스레드·런타임에서 돈다(브로커 알림은 런타임을 가리지 않는다).
pub fn spawn() {
    let _ = std::thread::Builder::new().name("approval-bridge".into()).spawn(|| {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(run());
    });
}

enum Note {
    Up(String, Up),
    Done(String),
}

async fn run() {
    let mut opened = claude_mod::subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Note>();
    let mut live: HashMap<String, Up> = HashMap::new();
    let mut tried: HashMap<String, std::time::Instant> = HashMap::new();
    loop {
        tokio::select! {
            got = opened.recv() => match got {
                Ok(request) => start(request, &mut tried, &tx),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return,
            },
            Some(note) = rx.recv() => match note {
                Note::Up(id, up) => { live.insert(id, up); }
                Note::Done(id) => { live.remove(&id); }
            },
            _ = tick.tick() => {
                // 알림을 놓친 요청(앱이 막 켜짐·밀림)도 올리고, 브로커에서 닫힌 요청은 관문에서도 닫는다.
                let open = claude_mod::permissions();
                for request in &open {
                    start(request.clone(), &mut tried, &tx);
                }
                let still: std::collections::HashSet<&str> = open.iter().filter_map(|r| r["id"].as_str()).collect();
                let gone: Vec<String> = live.keys().filter(|id| !still.contains(id.as_str())).cloned().collect();
                for id in gone {
                    if let Some(up) = live.remove(&id) {
                        tokio::spawn(async move {
                            let _ = gateway::cancel(&up.gateway, "local").await;
                        });
                    }
                }
                tried.retain(|_, at| at.elapsed() < Duration::from_secs(15 * 60));
            }
        }
    }
}

/// 관문에 올릴 요청인가. 질문(AskUserQuestion)은 허락·거절로 답하는 일이 아니라 올리지 않는다 — 올리면 폰·다른 맥에
/// [허락]·[거절] 창이 뜨고, 따로 가는 「질문 기다림」 알림과 두 번 울린다. 그 알림을 누르면 학생 칸이 열려 거기서 고른다.
fn remote(request: &Value) -> bool {
    request["tool"].as_str() != Some("AskUserQuestion")
}

/// 요청마다 한 번 — 올리고 결정을 따라가는 일을 따로 돌린다(관문이 느려도 다른 요청을 막지 않게).
fn start(request: Value, tried: &mut HashMap<String, std::time::Instant>, tx: &tokio::sync::mpsc::UnboundedSender<Note>) {
    let id = request["id"].as_str().unwrap_or_default().to_string();
    if id.is_empty() || tried.contains_key(&id) || !remote(&request) {
        return;
    }
    tried.insert(id.clone(), std::time::Instant::now());
    let tx = tx.clone();
    tokio::spawn(async move {
        let Some(up) = upload(&request).await else { return };
        let _ = tx.send(Note::Up(id.clone(), up.clone()));
        follow(&id, &up).await;
        let _ = tx.send(Note::Done(id));
    });
}

async fn upload(request: &Value) -> Option<Up> {
    let surface = request["surface"].as_str().unwrap_or_default().to_string();
    let session = request["session"].as_str().unwrap_or_default().to_string();
    let student = crate::character::session_character(&session).unwrap_or_default();
    let created = gateway::create(
        &student,
        &surface,
        request["cwd"].as_str().unwrap_or_default(),
        request["tool"].as_str().unwrap_or_default(),
        &request["input"],
        request["input_truncated"] == true,
        None,
    )
    .await;
    let view = match created {
        Ok(view) => view,
        Err(error) => {
            let code = error.to_string();
            if !matches!(code.as_str(), "signed_out" | "isolated_run") {
                eprintln!("[approval-bridge] 관문에 못 올림: {code}");
            }
            return None;
        }
    };
    Some(Up {
        gateway: view["id"].as_str()?.to_string(),
        digest: view["digest"].as_str().unwrap_or_default().to_string(),
        surface,
        session,
    })
}

/// 관문 결정을 기다려 브로커에 넘긴다. 만료·닫힘이면 아무것도 안 한다(엔진 창이 답한다).
async fn follow(local: &str, up: &Up) {
    let mut misses = 0;
    loop {
        let view = match gateway::wait_one(&up.gateway, 25).await {
            Ok(view) => view,
            Err(_) => {
                misses += 1;
                if misses > 8 {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        misses = 0;
        let state = view["state"].as_str().unwrap_or_default();
        if state == "pending" {
            continue;
        }
        let Some(decision) = (match state {
            "allowed" => Some("allow"),
            "denied" => Some("deny"),
            _ => None,
        }) else {
            return;
        };
        // 올린 글과 결정한 글이 같아야 한다 — 관문이 중간에 바꿔치지 않았나.
        if view["digest"].as_str() != Some(up.digest.as_str()) {
            eprintln!("[approval-bridge] 지문이 달라 결정을 버림: {local}");
            return;
        }
        let place = view["by"]["label"].as_str().unwrap_or("다른 기기");
        let by = format!("{place} · 원격");
        let message = if decision == "deny" { format!("사용자가 {place}에서 이 요청을 거절했어요") } else { String::new() };
        if let Err(error) = claude_mod::decide(local, &up.surface, &up.session, decision, &message, &by) {
            eprintln!("[approval-bridge] 브로커가 결정을 안 받음({error}): {local}");
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_stay_off_the_gateway() {
        assert!(!remote(&serde_json::json!({"id": "q", "tool": "AskUserQuestion"})));
        for tool in ["Bash", "Edit", "ExitPlanMode", ""] {
            assert!(remote(&serde_json::json!({"id": "t", "tool": tool})), "{tool}");
        }
    }
}
