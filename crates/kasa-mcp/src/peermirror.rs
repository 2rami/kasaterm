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
    /// 세울 때의 원격 세션 이름(라벨 붙이기 전) — 원격이 개명하면 유령을 다시
    /// 세우는 비교 기준. 안 따라가면 상대는 옛 이름으로만 불린다(2026-09-01 실측:
    /// 앱 재시작 직후 자동 슬러그로 굳어, 나중에 붙은 진짜 세션 이름으로 보낸
    /// 답장이 no agent 로 떨어졌다).
    name: String,
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

/// 살아 있는 미러 전부 — 키는 `(경로표식, remote_sid)`. 경로표식은 직결이면
/// `direct:<base>`, 릴레이면 `relay:<machine>` — 같은 세션이 두 경로로 잡혀도
/// 키가 갈려 둘 다 서는 일이 없게 sync 쪽에서 직결을 우선한다.
type Ghosts = HashMap<(String, String), Ghost>;

/// 유령이 원격으로 전달하는 길 — 직결(기계 명부의 base 로 직접) 또는 릴레이 경유.
#[derive(Clone, Debug)]
enum Route {
    /// 그 기계의 /term/message 로 직접.
    Direct { base: String },
    /// 중계소의 /relay/send 로 — 설정은 전달 시점에 다시 읽는다(토큰 회전 대비).
    Relay,
}

fn sessions_dir() -> Option<PathBuf> {
    Some(kasa_socket::home_dir()?.join(".claude/sessions"))
}

/// 이 소켓이 우리가 세운 유령의 프록시 소켓인가. 유령을 다시 명부·릴레이에
/// 광고하면 **메아리 루프**가 된다 — B의 세션을 A가 유령으로 세웠는데 A가 그
/// 유령을 자기 세션이라고 광고하면, B가 자기 세션의 유령을 또 세우고 메시지가
/// 제자리를 돈다. 광고 경로(peer_registry_get·릴레이 등록) 둘 다 이걸로 거른다.
pub(crate) fn is_ghost_socket(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("kasa-ghost-"))
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
fn forward_line(route: &Route, remote_sid: &str, line: &str) {
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
    let res = match route {
        // 직결 — 발신 사람·기계는 같은 계정·내 기계 사이라 비운다.
        Route::Direct { base } => {
            crate::remote::send_peer_message(base, remote_sid, &from_name, "", "", &body, None)
        }
        // 릴레이 — 설정을 다시 읽어(회전 대비) 내 계정·기계를 달아 보낸다.
        // 계정이 다른 상대에게는 릴레이가 외부 표식을 강제한다.
        Route::Relay => match relay_conf() {
            Some(conf) => crate::remote::relay_send(
                &conf.base,
                conf.token().as_deref(),
                remote_sid,
                &from_name,
                &conf.account,
                &conf.machine_id,
                &body,
            ),
            None => Err(anyhow::anyhow!("릴레이 설정(relay.json)이 사라졌어요")),
        },
    };
    if let Err(e) = res {
        eprintln!("[peermirror] 전달 실패 {route:?} {remote_sid}: {e:#}");
    }
}

// --- 릴레이 설정 ------------------------------------------------------------

/// `~/.config/kasaterm/relay.json` — 있으면 이 기계는 중계소에 자기 세션을 올리고
/// 중계소 명단의 남의 세션을 유령으로 세운다. 없으면 릴레이 경유는 통째로 꺼진다
/// (machines.json 직결과 같은 게이팅 규율).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RelayConf {
    /// 중계소 주소 (예: http://127.0.0.1:8790).
    pub base: String,
    /// X-Relay-Token 값 자체. token_file 과 둘 중 하나.
    token: Option<String>,
    token_file: Option<String>,
    /// 이 기계의 라벨(중계소 명단에 machine 으로 뜬다. 예: 맥북).
    pub machine_id: String,
    /// 이 기계의 계정 — 같은 계정끼리는 지시, 다르면 릴레이가 부탁 봉투를 강제.
    pub account: String,
    /// 중계소가 「이 기계로 배달할 때」 칠 주소(중계소 입장에서 닿는 주소).
    pub advertise_base: String,
    advertise_token: Option<String>,
    advertise_token_file: Option<String>,
}

impl RelayConf {
    pub fn token(&self) -> Option<String> {
        resolve_secret(&self.token, &self.token_file)
    }
    pub fn advertise_token(&self) -> Option<String> {
        resolve_secret(&self.advertise_token, &self.advertise_token_file)
    }
}

/// 값이 있으면 값, 없으면 파일에서 읽는다(~ 확장). 둘 다 없으면 None.
fn resolve_secret(value: &Option<String>, file: &Option<String>) -> Option<String> {
    if let Some(v) = value.as_ref().filter(|s| !s.is_empty()) {
        return Some(v.clone());
    }
    let f = file.as_ref().filter(|s| !s.is_empty())?;
    let path = if let Some(rest) = f.strip_prefix("~/") {
        kasa_socket::home_dir()?.join(rest)
    } else {
        PathBuf::from(f)
    };
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// env `KASATERM_RELAY`(JSON 통째) 가 먼저 — 검증용 rig 이 사용자 설정을 안 건드리고
/// 자기 릴레이·광고 주소를 가리키기 위해서다(다른 격리 env 와 같은 규율).
pub(crate) fn relay_conf() -> Option<RelayConf> {
    if let Ok(s) = std::env::var("KASATERM_RELAY") {
        if let Some(c) = parse_relay_conf(&s) {
            return Some(c);
        }
    }
    let home = kasa_socket::home_dir()?;
    let path = home.join(".config/kasaterm/relay.json");
    parse_relay_conf(&std::fs::read_to_string(path).ok()?)
}

/// 필수: base·machine_id·account·advertise_base. 하나라도 비면 None — 반쪽 설정으로
/// 등록만 되고 배달이 안 되는 유령 상태를 만들지 않는다.
fn parse_relay_conf(text: &str) -> Option<RelayConf> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).map(str::to_string);
    let req = |k: &str| s(k).filter(|x| !x.is_empty());
    Some(RelayConf {
        base: req("base")?.trim_end_matches('/').to_string(),
        token: s("token"),
        token_file: s("token_file"),
        machine_id: req("machine_id")?,
        account: req("account")?,
        advertise_base: req("advertise_base")?.trim_end_matches('/').to_string(),
        advertise_token: s("advertise_token"),
        advertise_token_file: s("advertise_token_file"),
    })
}

/// 이 기계의 살아 있는 실세션(유령 제외) — 릴레이에 올릴 목록. 명부 json 을 직접
/// 읽는다(peers::read_registry 는 status 를 버리는데 보드엔 상태가 실려야 해서).
fn local_live_sessions() -> Vec<serde_json::Value> {
    let Some(dir) = sessions_dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()?.to_str()? != "json" {
                return None;
            }
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            let sock = PathBuf::from(v.get("messagingSocketPath")?.as_str()?);
            // 소켓이 죽은 세션·우리가 세운 유령은 올리지 않는다(유령은 메아리 루프).
            if !sock.exists() || is_ghost_socket(&sock) {
                return None;
            }
            Some(serde_json::json!({
                "sid": v.get("sessionId")?.as_str()?,
                "name": v.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                "status": v.get("status").and_then(|s| s.as_str()).unwrap_or(""),
            }))
        })
        .collect()
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
fn spawn_ghost(route: Route, remote_sid: &str, name: &str, label: &str) -> std::io::Result<Ghost> {
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
        "cwd": kasa_socket::home_dir().map(|p| p.display().to_string()).unwrap_or_default(),
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
        let (remote_sid, stop) = (remote_sid.to_string(), stop.clone());
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
                                forward_line(&route, &remote_sid, line);
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

    Ok(Ghost { shell, json_path, key_path, sock_path, stop, name: name.to_string() })
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
/// 5초마다 ①기계 명부(machines.json)의 각 기계 `/peer-registry` ②릴레이
/// (relay.json — 자기 세션 등록 후 명단 수신) 를 받아 유령을 동기화한다.
/// 명부·릴레이 설정이 둘 다 비면 아무 일도 안 한다.
#[cfg(unix)]
pub fn spawn() {
    std::thread::Builder::new()
        .name("peermirror".into())
        .spawn(|| {
            sweep_orphan_ghosts();
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

/// 부팅 시 고아 유령 청소 — 앞선 카사텀이 강제종료·크래시로 죽으면 `Drop` 이 못
/// 돌아 유령 파일·껍데기 sleep 이 남고, 껍데기 pid 가 살아 있는 한 죽은 원격이
/// ListAgents 에 영영 뜬다(2026-09-01 실측: SIGTERM 만으로 잔재 발생). 유령은
/// 소켓 경로(kasa-ghost-*)로 정확히 가려내고, 껍데기는 **그 pid 가 정말 sleep 일
/// 때만** 죽인다 — pid 재사용으로 남의 프로세스를 잡는 사고 방지.
#[cfg(unix)]
fn sweep_orphan_ghosts() {
    let Some(dir) = sessions_dir() else { return };
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(sock) = v.get("messagingSocketPath").and_then(|s| s.as_str()) else { continue };
        let sock = PathBuf::from(sock);
        if !is_ghost_socket(&sock) {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if let Ok(pid) = stem.parse::<u32>() {
            let comm = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();
            if comm.rsplit('/').next() == Some("sleep") {
                let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
            }
        }
        let _ = std::fs::remove_file(&sock);
        // 명부 json 과 그 짝 .key 파일까지.
        let _ = std::fs::remove_file(&p);
        if let Ok(rd2) = std::fs::read_dir(&dir) {
            for k in rd2.flatten() {
                let name = k.file_name().to_string_lossy().to_string();
                if name.starts_with(&format!("{stem}.")) && name.ends_with(".key") {
                    let _ = std::fs::remove_file(k.path());
                }
            }
        }
    }
}

/// 릴레이 명단에서 유령으로 세울 대상 고르기 — 순수 함수라 테스트한다.
/// 거르는 것 둘: ①내 기계(machine == 나) — 내 세션을 유령으로 세우면 자기 메아리
/// ②직결 기계(machines.json 라벨과 같은 machine) — 직결 유령이 이미 서므로 릴레이
/// 유령까지 서면 같은 세션이 두 이름으로 뜬다. 직결이 우선이다(왕복이 한 홉 짧다).
fn relay_targets(
    rows: &[(String, String, String)], // (machine, sid, name)
    my_machine: &str,
    direct_labels: &[String],
) -> Vec<(String, String, String)> {
    rows.iter()
        .filter(|(machine, _, _)| {
            machine != my_machine && !direct_labels.iter().any(|l| l == machine)
        })
        .cloned()
        .collect()
}

#[cfg(unix)]
fn sync_once(ghosts: &Arc<Mutex<Ghosts>>) {
    let machines = crate::machines::machines();
    // 이번에 살아 있어야 할 유령 집합 — 키는 (경로표식, sid), 값은 (이름, 라벨, 경로).
    let mut want: HashMap<(String, String), (String, String, Route)> = HashMap::new();
    for m in &machines {
        match crate::remote::fetch_peer_registry(&m.base, None) {
            Ok(peers) => {
                for (sid, name) in peers {
                    want.insert(
                        (format!("direct:{}", m.base), sid),
                        (name, m.label.clone(), Route::Direct { base: m.base.clone() }),
                    );
                }
            }
            // 죽은 기계는 조용히 건너뛴다 — 그 기계 유령은 아래에서 걷힌다.
            Err(_) => {}
        }
    }
    // 릴레이 — 설정이 있으면 ①내 실세션을 등록하고 ②명단의 남의 기계 세션을 유령 후보로.
    if let Some(conf) = relay_conf() {
        let token = conf.token();
        if let Err(e) = crate::remote::relay_register(
            &conf.base,
            token.as_deref(),
            &conf.machine_id,
            &conf.account,
            &conf.advertise_base,
            conf.advertise_token().as_deref(),
            &local_live_sessions(),
        ) {
            eprintln!("[peermirror] 릴레이 등록 실패 {}: {e:#}", conf.base);
        }
        match crate::remote::relay_sessions(&conf.base, token.as_deref()) {
            Ok(rows) => {
                let direct_labels: Vec<String> =
                    machines.iter().map(|m| m.label.clone()).collect();
                for (machine, sid, name) in
                    relay_targets(&rows, &conf.machine_id, &direct_labels)
                {
                    want.insert(
                        (format!("relay:{machine}"), sid),
                        (name, machine, Route::Relay),
                    );
                }
            }
            Err(e) => eprintln!("[peermirror] 릴레이 명단 실패 {}: {e:#}", conf.base),
        }
    }
    let mut g = ghosts.lock().unwrap();
    // 사라진 것 제거(Drop 이 껍데기·파일·소켓 정리).
    let dead: Vec<(String, String)> =
        g.keys().filter(|k| !want.contains_key(*k)).cloned().collect();
    for k in dead {
        g.remove(&k);
    }
    // 새로 생긴 것 추가 + 개명 따라가기.
    for (key, (name, label, route)) in want {
        match g.get(&key) {
            // 이름이 같으면 그대로. 다르면 걷고 새 이름으로 다시 세운다 — 앱 재시작
            // 직후엔 자동 슬러그였다가 곧 진짜 세션 이름이 붙는데, 유령이 그걸 안
            // 따라가면 상대는 옛 이름으로만 불린다(실측: 답장이 no agent 로 실패).
            Some(existing) if existing.name == name => continue,
            Some(_) => {
                g.remove(&key);
            }
            None => {}
        }
        match spawn_ghost(route, &key.1, &name, &label) {
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

    #[test]
    fn ghost_sockets_are_recognized_and_excluded() {
        use std::path::Path;
        assert!(is_ghost_socket(Path::new("/tmp/cc-socks/kasa-ghost-1234.sock")));
        // 실세션 소켓·다른 파일은 유령이 아니다.
        assert!(!is_ghost_socket(Path::new("/tmp/cc-socks/48211.sock")));
        assert!(!is_ghost_socket(Path::new("/tmp/other/kasaterm-1.sock")));
    }

    #[test]
    fn relay_conf_parses_and_rejects_halves() {
        let full = r#"{"base":"http://127.0.0.1:8790/","token_file":"~/.config/kasaterm/relay-token",
            "machine_id":"맥북","account":"geno",
            "advertise_base":"http://127.0.0.1:18801/","advertise_token":"tok"}"#;
        let c = parse_relay_conf(full).expect("완전한 설정은 파싱돼야");
        assert_eq!(c.base, "http://127.0.0.1:8790"); // 꼬리 슬래시 제거
        assert_eq!(c.machine_id, "맥북");
        assert_eq!(c.account, "geno");
        assert_eq!(c.advertise_base, "http://127.0.0.1:18801");
        assert_eq!(c.advertise_token(), Some("tok".into()));
        // 필수 하나라도 빠지면 통째로 None — 반쪽 설정으로 등록만 되고 배달 안 되는
        // 유령 상태를 만들지 않는다.
        for broken in [
            r#"{"machine_id":"맥북","account":"geno","advertise_base":"http://x"}"#,
            r#"{"base":"http://x","account":"geno","advertise_base":"http://x"}"#,
            r#"{"base":"http://x","machine_id":"맥북","advertise_base":"http://x"}"#,
            r#"{"base":"http://x","machine_id":"맥북","account":"geno"}"#,
            "not json",
        ] {
            assert!(parse_relay_conf(broken).is_none(), "{broken} 이 통과했다");
        }
    }

    #[test]
    fn relay_targets_skip_self_and_direct_machines() {
        let rows = vec![
            ("맥북".to_string(), "s1".to_string(), "나".to_string()),
            ("맥미니".to_string(), "s2".to_string(), "미니학생".to_string()),
            ("데스크탑".to_string(), "s3".to_string(), "데탑학생".to_string()),
        ];
        // 내 기계(맥북)와 직결(맥미니)은 걸러지고 릴레이 전용(데스크탑)만 남는다.
        let t = relay_targets(&rows, "맥북", &["맥미니".to_string()]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].0, "데스크탑");
        // 직결이 없으면 내 것만 빠진다.
        let t2 = relay_targets(&rows, "맥북", &[]);
        assert_eq!(t2.len(), 2);
    }
}
