//! 원격 기계의 claude 세션을 이 기계에 **유령**으로 미러링한다 — 그러면 로컬
//! ListAgents 에 원격 세션이 뜨고, 로컬 claude 의 SendMessage 가 그 유령 소켓에
//! JSON 을 꽂으면 이 모듈이 `send_peer_message` 로 원격에 전달한다(2026-08-31
//! 유령 세션 실증으로 확정한 경로). claude 를 한 줄도 안 고친다.
//!
//! 실증으로 확정한 제약(ghost-probe 실험):
//! - **claude 는 명부 파일명의 pid 가 살아 있는지 검사한다**(죽은 pid·파일명만
//!   숫자인 유령은 안 뜬다). 그래서 유령마다 **살아 있는 껍데기 프로세스** 하나를
//!   붙이고, 그 pid 로 파일명을 짓는다. 껍데기는 `sleep` — 메모리 거의 0.
//! - 명부 파일명 규격: `<pid>.json` + `<pid>.<64hex>.key`. pid=숫자여야 한다.
//! - 소켓에 꽂히는 것: `{msgV,msg_id,type:"user",message.content=<cross-session
//!   -message …>,from}` 한 줄. 우리는 그 소켓의 **listen 쪽**이 되어 받는다.
//!
//! 수명: 카사텀이 살아 있는 동안만. 종료 시 `Mirror::drop` 이 껍데기를 kill 하고
//! 유령 파일·소켓을 지운다 — 안 지우면 죽은 원격이 로컬 ListAgents 에 영영 남는다.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 유령 하나 — 로컬 껍데기·파일·소켓의 수명 묶음. 원격 (base,sid)는 Ghosts 맵의
/// 키가 쥐고, 전달은 프록시 스레드가 캡처한 값으로 하므로 여기엔 안 둔다.
struct Ghost {
    /// 껍데기 프로세스(pid liveness 용). Drop 에서 kill.
    shell: std::process::Child,
    /// 이 유령의 명부 파일 둘·프록시 소켓 경로(정리용).
    json_path: PathBuf,
    key_path: PathBuf,
    sock_path: PathBuf,
    /// 프록시 소켓 리스너를 멈추는 신호.
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Ghost {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.shell.kill();
        let _ = self.shell.wait();
        let _ = std::fs::remove_file(&self.json_path);
        let _ = std::fs::remove_file(&self.key_path);
        let _ = std::fs::remove_file(&self.sock_path);
    }
}

/// 살아 있는 미러 전부 — 키는 `(base, remote_sid)`.
type Ghosts = HashMap<(String, String), Ghost>;

fn sessions_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude/sessions"))
}

/// 로컬 세션명(발신자 표시) — 유령 소켓에 온 JSON 의 `from` 소켓 경로로 되짚는다.
/// 못 찾으면 태그의 from-name 을 그대로 쓴다.
fn from_name_of_socket(from: &str) -> Option<String> {
    // `uds:/tmp/cc-socks/<pid>.sock` → pid
    let pid: u32 = from.rsplit('/').next()?.strip_suffix(".sock")?.parse().ok()?;
    let path = sessions_dir()?.join(format!("{pid}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v.get("name").and_then(|n| n.as_str()).map(str::to_string)
}

/// 유령 소켓에 꽂힌 한 줄(claude SendMessage)을 원격으로 전달한다.
fn forward_line(base: &str, remote_sid: &str, line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    // 본문은 message.content 의 <cross-session-message> 태그 안. 태그를 벗겨
    // 순수 본문만 넘긴다 — 받는 쪽(term_message_post)이 새 태그를 다시 씌운다.
    let content = v
        .pointer("/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let body = strip_cross_session_tag(content);
    let from = v.get("from").and_then(|f| f.as_str()).unwrap_or("");
    let from_name = from_name_of_socket(from).unwrap_or_else(|| {
        // 태그 속성 from-name 폴백.
        content
            .split_once("from-name=\"")
            .and_then(|(_, r)| r.split_once('"'))
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| "peer".to_string())
    });
    // 발신 사람·기계는 1단계(같은 계정·내 기계)에선 비운다 — 3단계에서 신원
    // 주입기가 채운다(from-person 자리는 term_message_post 가 이미 받는다).
    if let Err(e) =
        crate::remote::send_peer_message(base, remote_sid, &from_name, "", "", &body, None)
    {
        eprintln!("[peermirror] 전달 실패 {base} {remote_sid}: {e:#}");
    }
}

/// `<cross-session-message …>본문</cross-session-message>` → 본문만.
fn strip_cross_session_tag(s: &str) -> String {
    let inner = match s.split_once('>') {
        Some((head, rest)) if head.contains("cross-session-message") => rest,
        _ => return s.trim().to_string(),
    };
    inner
        .rsplit_once("</cross-session-message>")
        .map(|(b, _)| b)
        .unwrap_or(inner)
        .trim()
        .to_string()
}

/// 유령의 로컬 표시 이름 — 기계 라벨을 괄호로 붙여 로컬 세션과 구분한다
/// (「이름 (맥미니)」). ⚠️ `@` 는 쓰지 마라: SendMessage 의 `to` 가 `이름@팀`
/// 으로 파싱해 유령을 주소로 못 받는다(2026-08-31 실측 — `@` 이름은 배달 자체가
/// 안 됐다). 괄호·공백은 통한다(`프롬프트 올라가기` 처럼). remoteboard 도 board
/// 원격 행의 peer_name 을 이 이름과 같게 맞춰야 board 를 읽는 쪽 SendMessage 가
/// 유령에 닿는다 — 그래서 pub(crate).
pub(crate) fn ghost_display_name(name: &str, label: &str) -> String {
    if label.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({label})")
    }
}

/// 유령 하나를 세운다 — 껍데기 spawn + 명부 파일 + 프록시 소켓 리스너.
#[cfg(unix)]
fn spawn_ghost(base: &str, remote_sid: &str, name: &str, label: &str) -> std::io::Result<Ghost> {
    use std::os::unix::net::UnixListener;

    // 껍데기 — 살아 있는 pid 하나(claude 의 liveness 검사 통과용). 아주 오래 잔다.
    let shell = std::process::Command::new("sleep")
        .arg("2000000000")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = shell.id();

    // 프록시 소켓 — 유령마다 하나. 경로에 pid 를 실어 겹치지 않게.
    let sock_dir = PathBuf::from("/tmp/cc-socks");
    let _ = std::fs::create_dir_all(&sock_dir);
    let sock_path = sock_dir.join(format!("kasa-ghost-{pid}.sock"));
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    listener.set_nonblocking(true)?;

    let dir = sessions_dir().ok_or_else(|| std::io::Error::other("HOME 없음"))?;
    let _ = std::fs::create_dir_all(&dir);
    let json_path = dir.join(format!("{pid}.json"));
    let key_path = dir.join(format!("{pid}.{}.key", hex32()));
    let procstart = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let display = ghost_display_name(name, label);
    let entry = serde_json::json!({
        "pid": pid, "sessionId": remote_sid,
        "cwd": std::env::var("HOME").unwrap_or_default(),
        "startedAt": now, "procStart": procstart, "version": "2.1.251",
        "peerProtocol": 1, "peerFeatures": ["notify_idle"],
        "kind": "interactive", "entrypoint": "cli", "pidDomain": "darwin",
        "messagingSocketPath": sock_path.to_string_lossy(),
        "name": display, "nameSince": now,
        "status": "idle", "updatedAt": now, "statusUpdatedAt": now,
    });
    std::fs::write(&json_path, serde_json::to_string(&entry).unwrap_or_default())?;
    std::fs::write(
        &key_path,
        serde_json::json!({ "peerToken": hex16(), "procStart": procstart, "pidDomain": "darwin" })
            .to_string(),
    )?;

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 프록시 리스너 스레드 — 유령 소켓에 꽂힌 줄을 원격으로 전달.
    {
        let (base, remote_sid, stop) =
            (base.to_string(), remote_sid.to_string(), stop.clone());
        std::thread::Builder::new()
            .name(format!("ghost-proxy-{pid}"))
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut conn, _)) => {
                            let mut buf = String::new();
                            let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                            let _ = conn.read_to_string(&mut buf);
                            for line in buf.lines().filter(|l| !l.trim().is_empty()) {
                                forward_line(&base, &remote_sid, line);
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(150));
                        }
                        Err(_) => break,
                    }
                }
            })
            .ok();
    }

    Ok(Ghost { shell, json_path, key_path, sock_path, stop })
}

fn hex32() -> String {
    let mut s = String::with_capacity(64);
    for _ in 0..64 {
        s.push(char::from_digit(fastrand() % 16, 16).unwrap());
    }
    s
}
fn hex16() -> String {
    let mut s = String::with_capacity(32);
    for _ in 0..32 {
        s.push(char::from_digit(fastrand() % 16, 16).unwrap());
    }
    s
}
/// 의존성 없는 약한 난수(파일명·토큰용 — 암호 강도 불필요).
fn fastrand() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut x = SEED.load(std::sync::atomic::Ordering::Relaxed);
    if x == 0 {
        x = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.store(x, std::sync::atomic::Ordering::Relaxed);
    (x & 0xffff_ffff) as u32
}

/// 미러링 폴러를 백그라운드로 띄운다 — 카사텀 부팅 때 한 번 부른다.
/// 5초마다 각 기계의 `/peer-registry` 를 받아 유령을 동기화한다(새 세션 추가,
/// 사라진 세션 제거). 기계 명부가 비면 아무 일도 안 한다.
#[cfg(unix)]
pub fn spawn() {
    std::thread::Builder::new()
        .name("peermirror".into())
        .spawn(|| {
            let ghosts: Arc<Mutex<Ghosts>> = Arc::new(Mutex::new(HashMap::new()));
            loop {
                sync_once(&ghosts);
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        })
        .ok();
}
#[cfg(not(unix))]
pub fn spawn() {}

#[cfg(unix)]
fn sync_once(ghosts: &Arc<Mutex<Ghosts>>) {
    let machines = crate::machines::machines();
    // 이번에 살아 있어야 할 (base, sid) 집합.
    let mut want: HashMap<(String, String), (String, String)> = HashMap::new(); // →(name,label)
    for m in &machines {
        match crate::remote::fetch_peer_registry(&m.base, None) {
            Ok(peers) => {
                for (sid, name) in peers {
                    want.insert((m.base.clone(), sid), (name, m.label.clone()));
                }
            }
            // 죽은 기계는 조용히 건너뛴다 — 그 기계 유령은 아래에서 걷힌다.
            Err(_) => {}
        }
    }
    let mut g = ghosts.lock().unwrap();
    // 사라진 것 제거(Drop 이 껍데기·파일·소켓 정리).
    let dead: Vec<(String, String)> =
        g.keys().filter(|k| !want.contains_key(*k)).cloned().collect();
    for k in dead {
        g.remove(&k);
    }
    // 새로 생긴 것 추가.
    for (key, (name, label)) in want {
        if g.contains_key(&key) {
            continue;
        }
        match spawn_ghost(&key.0, &key.1, &name, &label) {
            Ok(ghost) => {
                g.insert(key, ghost);
            }
            Err(e) => eprintln!("[peermirror] 유령 생성 실패 {}: {e}", key.1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_cross_session_tag_to_body() {
        let s = "<cross-session-message from=\"x\" from-name=\"a\">\n안녕 본문\n</cross-session-message>";
        assert_eq!(strip_cross_session_tag(s), "안녕 본문");
        // 태그가 없으면 그대로(trim).
        assert_eq!(strip_cross_session_tag("  그냥 글  "), "그냥 글");
    }

    #[test]
    fn hex_lengths_match_registry_spec() {
        assert_eq!(hex32().len(), 64);
        assert_eq!(hex16().len(), 32);
        assert!(hex32().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ghost_name_never_contains_at_sign() {
        // `@` 는 SendMessage 를 `이름@팀` 으로 오해시켜 배달을 죽인다.
        let d = ghost_display_name("ghost-remote-test", "맥미니");
        assert_eq!(d, "ghost-remote-test (맥미니)");
        assert!(!d.contains('@'));
        // 라벨이 비면 이름 그대로.
        assert_eq!(ghost_display_name("solo", ""), "solo");
    }
}
