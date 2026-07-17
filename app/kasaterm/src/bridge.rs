//! bg SendMessage 브리지 — detach 된 background claude 세션은 teammate 플래그가
//! 유실돼(데몬이 argv 를 재구성한 포크) 인박스 폴러가 없다. 이 브리지가 kt-* 팀
//! 인박스의 미읽음 메시지를 찾아 `claude attach` pty 로 원문을 직접 주입한다.
//!
//! attach 입력 경로의 실측 함정 두 가지가 설계를 결정한다:
//! - 클라이언트는 spawn 한 쪽이 pty 출력을 소비해줘야 진행한다. TUI 는 글자마다
//!   전체 리렌더라 긴 send 중 커널 pty 버퍼(16KB)가 차면 클라이언트 write 가
//!   블록돼 입력 flush 가 유실된다 → 8자 청크 send + 청크마다 드레인 필수.
//! - 중첩 claude env(CLAUDECODE 등)가 있으면 attach 가 새 세션 TUI 로 폴백한다
//!   → env_clear 후 최소 env 만 준다.
//!
//! 배달 판정은 결정론: 주입 텍스트에 nonce(msg_id 앞 8자)를 심고 대상 세션의
//! transcript jsonl 에 등장해야 성공 → 그때만 인박스 read:true 마킹. busy 세션은
//! 입력이 큐잉돼 착지가 늦을 수 있어, 매 tick 배달 전에 nonce 기착지를 먼저
//! 확인해(이미 있으면 마킹만) 이중 배달을 막는다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(5);
const BACKOFF: Duration = Duration::from_secs(90);

const ATTACH_EXPECT: &str = r#"#!/usr/bin/expect -f
set timeout 25
set bin   [lindex $argv 0]
set sid   [lindex $argv 1]
set guard [lindex $argv 2]
set nonce [lindex $argv 3]
set fh [open [lindex $argv 4] r]
set msg [string trim [read $fh]]
close $fh
spawn -noecho $bin attach $sid
if {$guard ne ""} {
    expect {
        -- "$guard" {}
        timeout { puts "\nKB-NOT-ATTACHED"; exit 3 }
    }
}
expect -timeout 3 timeout {}
# 조용창 2s — 출력이 오면(사용자 타이핑 에코/렌더) 입력창 충돌 위험이라 물러난다.
# 다음 tick 재시도(짧은 재시도, 90s 백오프 아님). 실사례: 주입이 거노 타이핑 중간에
# 끼어들어 "방향키" 사이에 [학생채팅 …] 이 박힌 채 한 턴으로 섞여 제출됨(07-17).
expect -timeout 2 -re "." { puts "\nKB-USER-ACTIVE"; exit 5 } timeout {}
set len [string length $msg]
for {set i 0} {$i < $len} {incr i 8} {
    send -- [string range $msg $i [expr {$i+7}]]
    expect -timeout 1 timeout {}
}
expect {
    -- "$nonce" {}
    timeout {
        # 미확인 입력을 지우고 나간다 — 잔류하면 다음 배달의 \r 에 합승 제출된다.
        send -- "\x15"
        expect -timeout 2 timeout {}
        puts "\nKB-NO-ECHO"
        exit 4
    }
}
send -- "\r"
expect -timeout 10 timeout {}
puts "\nKB-SENT"
close
"#;

/// resumed() 의 bg-agents 폴러 옆에서 한 번 스폰. `bg_agents` 는 그 폴러가 채우는
/// sessionId→parentSessionId 맵(background kind 만)을 읽기 전용으로 공유받는다.
pub(crate) fn spawn_inbox_bridge(bg_agents: Arc<Mutex<HashMap<String, Option<String>>>>) {
    if !Path::new("/usr/bin/expect").exists() {
        return;
    }
    std::thread::spawn(move || {
        let bin = kasa_mcp::claude_bin();
        let mut backoff: HashMap<String, Instant> = HashMap::new();
        loop {
            std::thread::sleep(TICK);
            let bg = match bg_agents.lock() {
                Ok(g) => g.clone(),
                Err(_) => break,
            };
            if bg.is_empty() {
                continue;
            }
            for (inbox, slug) in scan_team_inboxes() {
                let Some(sid4) = slug_sid4(&slug) else { continue };
                // 직접 매칭 → 없으면 죽은 부모의 후계 포크(parent 가 sid4 로 시작)
                let target = bg
                    .keys()
                    .find(|s| s.starts_with(&sid4))
                    .or_else(|| {
                        bg.iter()
                            .find(|(_, p)| {
                                p.as_deref().is_some_and(|p| p.starts_with(&sid4))
                            })
                            .map(|(k, _)| k)
                    })
                    .cloned();
                let Some(sid) = target else { continue };
                if backoff
                    .get(&sid)
                    .is_some_and(|t| t.elapsed() < BACKOFF)
                {
                    continue;
                }
                let Some((pending, notices)) = unread_messages(&inbox) else {
                    continue;
                };
                // idle_notification 은 배달 대상이 아니다 — 마킹만 해서 tick 마다
                // 다시 걸리지 않게 한다.
                if !notices.is_empty() {
                    mark_read(&inbox, &notices);
                }
                if pending.is_empty() {
                    continue;
                }
                let nonce = pending
                    .last()
                    .map(|m| m.id.chars().take(8).collect::<String>())
                    .unwrap_or_default();
                if nonce.is_empty() {
                    continue;
                }
                let ids: Vec<String> = pending.iter().map(|m| m.id.clone()).collect();
                // 직전 배달이 큐잉으로 늦게 착지한 경우 — 재주입 없이 마킹만.
                if transcript_contains(&sid, &nonce) {
                    mark_read(&inbox, &ids);
                    continue;
                }
                let text = format_delivery(&pending, &nonce);
                match deliver(&bin, &sid, &text, &nonce) {
                    DeliverOutcome::Sent => {
                        mark_read(&inbox, &ids);
                        backoff.remove(&sid);
                    }
                    // 사용자 활동 감지 — 백오프 없이 다음 tick 재시도(충돌 회피).
                    DeliverOutcome::UserActive => {}
                    DeliverOutcome::Failed => {
                        backoff.insert(sid, Instant::now());
                    }
                }
            }
        }
    });
}

struct InboxMsg {
    id: String,
    from: String,
    text: String,
}

/// `~/.claude/teams/kt-*/inboxes/*.json` 전수 → (파일경로, 슬러그).
fn scan_team_inboxes() -> Vec<(PathBuf, String)> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let root = Path::new(&home).join(".claude/teams");
    let mut out = Vec::new();
    let Ok(teams) = std::fs::read_dir(&root) else {
        return out;
    };
    for team in teams.flatten() {
        if !team.file_name().to_string_lossy().starts_with("kt-") {
            continue;
        }
        let Ok(inboxes) = std::fs::read_dir(team.path().join("inboxes")) else {
            continue;
        };
        for f in inboxes.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push((p.clone(), stem.to_string()));
            }
        }
    }
    out
}

/// 자동 팀모드 슬러그 `<로마자>-<sid4>` 의 꼬리 4hex. team-lead 등은 None.
fn slug_sid4(slug: &str) -> Option<String> {
    let tail = slug.rsplit('-').next()?;
    if tail.len() == 4 && tail.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(tail.to_ascii_lowercase())
    } else {
        None
    }
}

/// (배달할 메시지, 마킹만 할 idle_notification id) — read:false 만.
fn unread_messages(path: &Path) -> Option<(Vec<InboxMsg>, Vec<String>)> {
    let raw = std::fs::read_to_string(path).ok()?;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&raw).ok()?;
    let mut pending = Vec::new();
    let mut notices = Vec::new();
    for m in &arr {
        if m.get("read").and_then(|r| r.as_bool()) != Some(false) {
            continue;
        }
        let id = m
            .get("msg_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let text = m
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if text.starts_with("{\"type\":\"idle_notification\"") {
            notices.push(id);
            continue;
        }
        pending.push(InboxMsg {
            id,
            from: m
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            text,
        });
    }
    Some((pending, notices))
}

/// 개행은 attach 입력에서 제출(\r)로 해석될 수 있어 평탄화한다. nonce 는 선두에
/// 둔다 — 입력박스가 긴 메시지를 줄바꿈하면 꼬리 토큰이 쪼개져 echo 매칭이
/// 깨지지만, 첫 줄 첫머리는 항상 통째로 렌더된다.
fn format_delivery(msgs: &[InboxMsg], nonce: &str) -> String {
    let body = msgs
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                m.from,
                m.text.replace(['\n', '\r'], " / ")
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    format!("[학생채팅 msg:{nonce}] {body}")
}

fn transcript_contains(sid: &str, nonce: &str) -> bool {
    let Some(p) = crate::socket::transcript_path_for_session(sid) else {
        return false;
    };
    std::fs::read_to_string(&p)
        .map(|raw| raw.contains(&format!("msg:{nonce}")))
        .unwrap_or(false)
}

/// 화면 리플레이에서 매칭할 대상 세션 고유 토큰. TUI 는 단어 사이를 커서 이동으로
/// 렌더해 여러 단어 조각은 매칭이 깨진다 — 공백 없는 단일 토큰만 쓴다. 못 찾으면
/// 빈 문자열(guard 없이 고정 대기)로 폴백; 폴백 오배달은 nonce 착지 판정이 막는다.
fn guard_token(sid: &str) -> String {
    let Some(p) = crate::socket::transcript_path_for_session(sid) else {
        return String::new();
    };
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return String::new();
    };
    for line in raw.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if !matches!(v.get("type").and_then(|t| t.as_str()), Some("user" | "assistant")) {
            continue;
        }
        let text = match v.pointer("/message/content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
            _ => continue,
        };
        for tok in text.split_whitespace().rev() {
            let clean: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            let n = clean.chars().count();
            if (4..=16).contains(&n) {
                return clean;
            }
        }
    }
    String::new()
}

fn bridge_tmp() -> PathBuf {
    let dir = std::env::temp_dir().join("kasaterm-bridge");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

enum DeliverOutcome {
    Sent,
    /// 대상 세션에 사용자 출력 활동(타이핑 에코 등) — 입력창 충돌 회피로 이번 tick 은
    /// 물러남. 백오프 없이 다음 tick 재시도.
    UserActive,
    Failed,
}

fn deliver(bin: &Path, sid: &str, text: &str, nonce: &str) -> DeliverOutcome {
    let sid8 = &sid[..sid.len().min(8)];
    let dir = bridge_tmp();
    let script = dir.join("attach-send.exp");
    if std::fs::read_to_string(&script).map(|c| c != ATTACH_EXPECT).unwrap_or(true)
        && std::fs::write(&script, ATTACH_EXPECT).is_err()
    {
        return DeliverOutcome::Failed;
    }
    let msgfile = dir.join(format!("msg-{sid8}.txt"));
    if std::fs::write(&msgfile, text).is_err() {
        return DeliverOutcome::Failed;
    }
    let guard = guard_token(sid);
    let home = std::env::var_os("HOME").unwrap_or_default();
    let out = std::process::Command::new("/usr/bin/expect")
        .arg(&script)
        .arg(bin)
        .arg(sid8)
        .arg(&guard)
        .arg(nonce)
        .arg(&msgfile)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TERM", "xterm-256color")
        .env("LANG", "ko_KR.UTF-8")
        .output();
    let _ = std::fs::remove_file(&msgfile);
    let stdout = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if stdout.contains("KB-USER-ACTIVE") {
        return DeliverOutcome::UserActive;
    }
    if !stdout.contains("KB-SENT") {
        return DeliverOutcome::Failed;
    }
    for _ in 0..5 {
        if transcript_contains(sid, nonce) {
            return DeliverOutcome::Sent;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    DeliverOutcome::Failed
}

fn mark_read(path: &Path, ids: &[String]) {
    // 발신자 append 와의 짧은 레이스 창구를 줄이려 읽기 직후 바로 쓴다.
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut arr) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) else {
        return;
    };
    let mut changed = false;
    for m in arr.iter_mut() {
        let hit = m
            .get("msg_id")
            .and_then(|v| v.as_str())
            .is_some_and(|id| ids.iter().any(|x| x == id));
        if hit && m.get("read").and_then(|r| r.as_bool()) == Some(false) {
            m["read"] = serde_json::Value::Bool(true);
            changed = true;
        }
    }
    if !changed {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(&arr) else {
        return;
    };
    let tmp = path.with_extension("json.kbtmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_sid4_accepts_only_hex_tail() {
        assert_eq!(slug_sid4("midori-2535"), Some("2535".into()));
        assert_eq!(slug_sid4("yuuka-3d51"), Some("3d51".into()));
        assert_eq!(slug_sid4("team-lead"), None);
        assert_eq!(slug_sid4("persona-roster"), None);
        assert_eq!(slug_sid4("solo"), None);
    }

    #[test]
    fn format_delivery_flattens_newlines_and_tags_nonce() {
        let msgs = vec![InboxMsg {
            id: "abc".into(),
            from: "yuuka-3d51".into(),
            text: "첫 줄\n둘째 줄".into(),
        }];
        let s = format_delivery(&msgs, "deadbeef");
        assert!(s.starts_with("[학생채팅 msg:deadbeef] yuuka-3d51: 첫 줄 / 둘째 줄"));
        assert!(!s.contains('\n'));
    }

    #[test]
    fn unread_messages_splits_notices_from_pending() {
        let dir = std::env::temp_dir().join("kb-bridge-test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("inbox.json");
        std::fs::write(
            &p,
            r#"[
              {"msg_id":"m1","from":"a","text":"hi","read":false,"type":"message"},
              {"msg_id":"m2","from":"a","text":"{\"type\":\"idle_notification\",\"x\":1}","read":false,"type":"message"},
              {"msg_id":"m3","from":"b","text":"old","read":true,"type":"message"}
            ]"#,
        )
        .unwrap();
        let (pending, notices) = unread_messages(&p).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "m1");
        assert_eq!(notices, vec!["m2".to_string()]);
        mark_read(&p, &["m1".into(), "m2".into()]);
        let (pending, notices) = unread_messages(&p).unwrap();
        assert!(pending.is_empty());
        assert!(notices.is_empty());
        let _ = std::fs::remove_file(&p);
    }
}
