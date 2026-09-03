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

/// 입력 청크 크기(문자). 8자씩 끊는 이유는 모듈 머리말 참고 — 한 번에 밀면 긴
/// send 중 pty 버퍼가 차서 클라이언트 write 가 막힌다. unix 는 같은 값이 expect
/// 스크립트 리터럴에 박혀 있어 이 상수를 안 읽는다(안 접으면 그쪽에서 dead_code).
#[cfg(windows)]
const CHUNK: usize = 8;

/// attach 춤의 단계별 시한. unix 는 expect 스크립트 리터럴에, Windows 는 직접
/// 구현(`attach_send`)에 같은 값이 들어간다 — 한쪽만 고치면 두 플랫폼의 배달
/// 판정이 조용히 갈라지니 `REAL` 을 정본으로 본다.
#[cfg(windows)]
#[derive(Clone, Copy)]
struct Timing {
    /// guard·nonce 가 화면에 뜨기를 기다리는 한도.
    attach: Duration,
    /// attach 직후 화면이 자리 잡기를 기다리는 시간.
    settle: Duration,
    /// 사용자 활동 감지 창 — 이 동안 출력이 오면 물러난다.
    quiet: Duration,
    /// 청크마다 pty 를 비워 주는 시간.
    chunk_drain: Duration,
    /// `\r` 제출 뒤 상대가 소화하기를 기다리는 시간.
    submit_drain: Duration,
    /// 에코를 못 봐 물러날 때, 지우기(`^U`)가 먹히기를 기다리는 시간.
    clear_drain: Duration,
}

#[cfg(windows)]
impl Timing {
    const REAL: Timing = Timing {
        attach: Duration::from_secs(25),
        settle: Duration::from_secs(3),
        quiet: Duration::from_secs(2),
        chunk_drain: Duration::from_secs(1),
        submit_drain: Duration::from_secs(10),
        clear_drain: Duration::from_secs(2),
    };
    /// 테스트용 — 같은 상태기계를 초 단위 대기 없이 돌린다. 시한은 정책이고
    /// 검증 대상은 순서(guard→조용창→청크 전송→에코 확인→제출)라서 줄여도 된다.
    #[cfg(test)]
    const FAST: Timing = Timing {
        attach: Duration::from_secs(2),
        settle: Duration::from_millis(150),
        quiet: Duration::from_millis(150),
        chunk_drain: Duration::from_millis(20),
        submit_drain: Duration::from_millis(200),
        clear_drain: Duration::from_millis(100),
    };
}

#[cfg(unix)]
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
    // unix 는 배달을 expect 에 맡기니 그게 없으면 시작할 이유가 없다. Windows 는
    // expect 가 아예 존재하지 않아(Git for Windows 도 안 준다) ConPTY 로 직접
    // 춤을 춘다 — 전제 조건이 없으므로 무조건 뜬다.
    #[cfg(unix)]
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
    // `HOME` 직독이 아니라 home_dir() — Windows GUI 프로세스엔 HOME 이 없어
    // 여기서 늘 빈 목록으로 빠져나갔다(브리지가 도는데 배달할 게 영영 없음).
    let Some(home) = kasa_socket::home_dir() else {
        return Vec::new();
    };
    let root = home.join(".claude/teams");
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
    format!("[캐릭터채팅 msg:{nonce}] {body}")
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

#[cfg(unix)]
fn bridge_tmp() -> PathBuf {
    let dir = std::env::temp_dir().join("kasaterm-bridge");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Debug)]
enum DeliverOutcome {
    Sent,
    /// 대상 세션에 사용자 출력 활동(타이핑 에코 등) — 입력창 충돌 회피로 이번 tick 은
    /// 물러남. 백오프 없이 다음 tick 재시도.
    UserActive,
    Failed,
}

#[cfg(unix)]
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
    confirm_landing(sid, nonce)
}

/// 배달 판정 정본 — 보냈다는 사실이 아니라 **대상 세션 transcript 에 nonce 가
/// 나타났는지**로만 성공을 정한다. busy 세션은 입력이 큐잉돼 착지가 늦으니 잠깐
/// 기다려 준다. 두 플랫폼이 같은 기준을 쓰도록 여기 하나로 모은다.
fn confirm_landing(sid: &str, nonce: &str) -> DeliverOutcome {
    for _ in 0..5 {
        if transcript_contains(sid, nonce) {
            return DeliverOutcome::Sent;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    DeliverOutcome::Failed
}

/// unix 의 expect 스크립트(`ATTACH_EXPECT`)와 **같은 춤**을 ConPTY 위에서 직접
/// 춘다 — Windows 엔 expect 가 없다(Git for Windows 도 안 준다). 단계와 시한은
/// 위 상수로 스크립트와 1:1 이고, 모듈 머리말의 실측 함정 둘도 그대로 지킨다:
/// 읽기 스레드가 쉬지 않고 pty 를 비워 클라이언트 write 가 막히지 않게 하고,
/// 입력은 8자 청크로 쪼개 청크마다 드레인한다.
#[cfg(windows)]
fn deliver(bin: &Path, sid: &str, text: &str, nonce: &str) -> DeliverOutcome {
    let sid8 = &sid[..sid.len().min(8)];
    let guard = guard_token(sid);
    let mut cmd = portable_pty::CommandBuilder::new(bin);
    cmd.arg("attach");
    cmd.arg(sid8);
    match attach_send(cmd, &guard, nonce, text, Timing::REAL) {
        DeliverOutcome::Sent => confirm_landing(sid, nonce),
        other => other,
    }
}

/// 춤 본체. 스폰할 커맨드를 통째로 받는 이유는 테스트가 진짜 claude 대신 에코하는
/// 셸을 물려 같은 경로를 그대로 돌리기 위해서다. 반환하는 `Sent` 는 "제출까지
/// 끝냈다"는 뜻이고 착지 판정은 아니다(그건 `confirm_landing`).
#[cfg(windows)]
fn attach_send(
    mut cmd: portable_pty::CommandBuilder,
    guard: &str,
    nonce: &str,
    text: &str,
    t: Timing,
) -> DeliverOutcome {
    use std::io::{Read, Write};

    // 중첩 claude env 가 남아 있으면 attach 가 새 세션 TUI 로 폴백한다(unix 실측).
    // unix 는 env_clear 후 최소 env 를 다시 주지만, Windows 에서 통째로 비우면
    // SystemRoot 가 사라져 자식이 아예 안 뜬다 — 문제되는 마커만 지운다.
    for k in crate::CLAUDE_MARKER_ENV {
        cmd.env_remove(k);
    }
    cmd.env("TERM", "xterm-256color");

    let size = portable_pty::PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 };
    let Ok(pair) = portable_pty::native_pty_system().openpty(size) else {
        return DeliverOutcome::Failed;
    };
    let Ok(mut child) = pair.slave.spawn_command(cmd) else {
        return DeliverOutcome::Failed;
    };
    // slave 를 쥐고 있으면 자식이 죽어도 reader 가 EOF 를 못 본다.
    drop(pair.slave);
    let (Ok(mut reader), Ok(writer)) =
        (pair.master.try_clone_reader(), pair.master.take_writer())
    else {
        let _ = child.kill();
        return DeliverOutcome::Failed;
    };
    // 쓰기단은 읽기 스레드와 나눠 쓴다 — 아래 DSR-CPR 응답이 거기서 나간다.
    let writer = Arc::new(Mutex::new(writer));

    // 화면은 통째로 모으고(TUI 는 매 글자 전체 리렌더라 조각 매칭이 깨진다),
    // 마지막 수신 시각도 같이 들고 있는다 — 사용자 활동 감지가 그걸 본다.
    let seen = Arc::new(Mutex::new((String::new(), Instant::now())));
    let pump = seen.clone();
    let answer = writer.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let chunk = String::from_utf8_lossy(&buf[..n]).into_owned();
            // ConPTY 는 붙자마자 DSR-CPR(`\e[6n`)을 던지고, 답이 없으면 자식이
            // **첫 프롬프트에 영영 도달하지 못한다**(kasa-pty 의 PtyEventForwarder
            // 가 같은 이유로 존재한다 — state.rs 의 주석). 여기선 화면을 파싱하지
            // 않으니 1;1 로 고정 응답한다. 묻는 쪽은 진행만 하면 되고, 우리는
            // 렌더를 안 하므로 좌표가 정확할 이유가 없다.
            if chunk.contains("\u{1b}[6n") {
                if let Ok(mut w) = answer.lock() {
                    let _ = w.write_all(b"\x1b[1;1R");
                    let _ = w.flush();
                }
            }
            let Ok(mut g) = pump.lock() else { break };
            g.0.push_str(&chunk);
            g.1 = Instant::now();
        }
    });

    let screen_has = |needle: &str| -> bool {
        seen.lock().map(|g| g.0.contains(needle)).unwrap_or(false)
    };
    let wait_for = |needle: &str, limit: Duration| -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if screen_has(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    };
    // 춤 본체는 클로저 안에서 끝내고, 어떤 갈래로 빠져나가든 정리는 아래 한 곳에서
    // 한 번만 한다 — attach 클라이언트가 살아남으면 다음 tick 이 같은 세션에 두
    // 번째로 붙는다.
    // 읽기 스레드가 DSR-CPR 응답으로 같은 쓰기단을 쓰므로 매번 잠근다.
    let put = |bytes: &[u8]| -> bool {
        let Ok(mut w) = writer.lock() else { return false };
        w.write_all(bytes).is_ok() && w.flush().is_ok()
    };
    let dance = || -> DeliverOutcome {
        if !guard.is_empty() && !wait_for(guard, t.attach) {
            return DeliverOutcome::Failed; // KB-NOT-ATTACHED
        }
        std::thread::sleep(t.settle);
        // 조용창 — 여기서 출력이 오면 사용자가 타이핑 중이라 입력창이 충돌한다.
        let quiet_from = Instant::now();
        std::thread::sleep(t.quiet);
        if seen.lock().map(|g| g.1 > quiet_from).unwrap_or(false) {
            return DeliverOutcome::UserActive;
        }
        let chars: Vec<char> = text.chars().collect();
        for part in chars.chunks(CHUNK) {
            let s: String = part.iter().collect();
            if !put(s.as_bytes()) {
                return DeliverOutcome::Failed;
            }
            std::thread::sleep(t.chunk_drain);
        }
        if !wait_for(nonce, t.attach) {
            // 미확인 입력을 지우고 나간다 — 잔류하면 다음 배달의 \r 에 합승 제출된다.
            put(b"\x15");
            std::thread::sleep(t.clear_drain);
            return DeliverOutcome::Failed; // KB-NO-ECHO
        }
        if !put(b"\r") {
            return DeliverOutcome::Failed;
        }
        std::thread::sleep(t.submit_drain);
        DeliverOutcome::Sent
    };
    let outcome = dance();
    if std::env::var_os("KASATERM_BRIDGE_DEBUG").is_some() {
        let screen = seen.lock().map(|g| g.0.clone()).unwrap_or_default();
        eprintln!("[bridge] outcome={outcome:?} screen={screen:?}");
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(pair.master);
    // reader 스레드는 join 하지 않고 놓아준다. ConPTY 출력단은 핸들이 전부 닫힐
    // 때까지 EOF 를 안 주는데 그 시점이 우리 손 밖이라, 기다리면 배달 하나가
    // 브리지 루프 전체를 멈춰 세운다. 스레드가 만지는 건 Arc 로 공유된 버퍼뿐이라
    // 남아 돌아도 해가 없다.
    drop(reader_thread);
    outcome
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
        assert!(s.starts_with("[캐릭터채팅 msg:deadbeef] yuuka-3d51: 첫 줄 / 둘째 줄"));
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

/// attach 춤 자체를 pty 양쪽 다 세워 놓고 돌린다. 진짜 claude 대신 **테스트
/// 바이너리 자신**을 pty 반대편에 올려(`fake_attach_child`) 붙이므로 expect·python
/// 같은 외부 도구가 필요 없고, ConPTY 경로가 실제로 물리는지까지 확인된다.
#[cfg(all(test, windows))]
mod attach_tests {
    use super::*;
    use std::io::{Read, Write};

    /// pty 반대편 역할. 부모가 `--exact <이 이름> --ignored --nocapture` 로 자기
    /// 자신을 다시 불러 세운다.
    ///
    /// 콘솔을 raw 로 내리는 게 핵심이다 — ConPTY 기본은 cooked 라 conhost 가
    /// 알아서 에코하고 read 가 개행까지 블록된다. 그러면 "에코가 안 온다" 갈래를
    /// 아예 못 만들고(콘솔이 대신 에코해 준다) 청크 단위 관찰도 안 된다. 진짜
    /// claude TUI 도 raw 로 내리므로 이쪽이 실제에 가깝다.
    #[test]
    #[ignore = "pty 반대편 역할 — attach_send 테스트가 스폰한다"]
    fn fake_attach_child() {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
            ENABLE_PROCESSED_INPUT, STD_INPUT_HANDLE,
        };
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            if GetConsoleMode(h, &mut mode) != 0 {
                mode &= !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
                SetConsoleMode(h, mode);
            }
        }
        let echo = std::env::var("KB_FAKE_ECHO").as_deref() != Ok("0");
        let chatty = std::env::var("KB_FAKE_CHATTY").as_deref() == Ok("1");
        let mut receipt = std::env::var("KB_FAKE_LOG")
            .ok()
            .map(|p| std::fs::File::create(p).unwrap());

        let guard = std::env::var("KB_FAKE_GUARD").unwrap_or_default();
        print!("{guard}\r\n");
        std::io::stdout().flush().unwrap();
        // 조용창을 일부러 깨뜨리는 모드 — 사용자가 타이핑 중인 세션을 흉내낸다.
        if chatty {
            for _ in 0..40 {
                print!(".");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        let mut stdin = std::io::stdin();
        let mut b = [0u8; 1];
        while let Ok(1) = stdin.read(&mut b) {
            if let Some(f) = receipt.as_mut() {
                f.write_all(&b).unwrap();
                f.flush().unwrap();
            }
            if echo {
                std::io::stdout().write_all(&b).unwrap();
                std::io::stdout().flush().unwrap();
            }
            if b[0] == b'\r' || b[0] == 0x15 {
                break;
            }
        }
    }

    /// 부모 쪽 준비 — 자기 자신을 가짜 attach 클라이언트로 세울 커맨드.
    fn fake_child(log: &Path, guard: &str) -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
        for a in [
            "--exact",
            "bridge::attach_tests::fake_attach_child",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ] {
            cmd.arg(a);
        }
        cmd.env("KB_FAKE_LOG", log);
        cmd.env("KB_FAKE_GUARD", guard);
        cmd
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("kb-bridge-attach");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn attach_send_waits_for_the_guard_then_chunks_the_text_and_submits() {
        let log = scratch("happy.txt");
        let text = "[캐릭터채팅 msg:abcd1234] 하나: 둘 | 셋: 넷";
        let out = attach_send(
            fake_child(&log, "KB-READY"),
            "KB-READY",
            "abcd1234",
            text,
            Timing::FAST,
        );
        assert!(matches!(out, DeliverOutcome::Sent), "제출까지 갔어야 한다");
        // 8자 청크로 쪼개 보냈어도 반대편이 받은 건 원문 그대로 + 제출 \r.
        let got = std::fs::read_to_string(&log).unwrap();
        assert_eq!(got, format!("{text}\r"));
    }

    /// 에코가 안 돌아오면 제출하지 않는다 — 미확인 입력을 `^U`(0x15)로 지우고
    /// 물러나야 다음 배달의 `\r` 에 합승 제출되지 않는다.
    #[test]
    fn attach_send_clears_the_input_instead_of_submitting_when_nothing_echoes() {
        let log = scratch("noecho.txt");
        let mut cmd = fake_child(&log, "KB-READY");
        cmd.env("KB_FAKE_ECHO", "0");
        let out = attach_send(cmd, "KB-READY", "abcd1234", "msg:abcd1234 본문", Timing::FAST);
        assert!(matches!(out, DeliverOutcome::Failed), "에코 없으면 실패다");
        let got = std::fs::read_to_string(&log).unwrap();
        assert!(got.ends_with('\u{15}'), "지우기로 끝나야 한다: {got:?}");
        assert!(!got.contains('\r'), "제출은 절대 하면 안 된다: {got:?}");
    }

    /// 조용창에 출력이 오면(사용자 타이핑 에코) 입력창 충돌을 피해 물러난다.
    /// 백오프 없이 다음 tick 재시도라 `Failed` 와 구분돼야 한다.
    #[test]
    fn attach_send_backs_off_when_the_session_is_still_talking() {
        let log = scratch("chatty.txt");
        let mut cmd = fake_child(&log, "KB-READY");
        cmd.env("KB_FAKE_CHATTY", "1");
        let out = attach_send(cmd, "KB-READY", "abcd1234", "msg:abcd1234 본문", Timing::FAST);
        assert!(matches!(out, DeliverOutcome::UserActive), "물러났어야 한다");
        // 한 글자도 안 보냈어야 한다 — 끼어들면 사용자 입력과 한 턴으로 섞인다.
        let got = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(got.is_empty(), "물러났는데 뭔가 보냈다: {got:?}");
    }
}
