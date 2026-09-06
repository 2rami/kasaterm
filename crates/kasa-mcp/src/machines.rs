//! 기계 명부 — 학생을 옮겨 다니게 할 「다른 기계」들의 정본.
//!
//! remoteboard(원격 kasaterm 의 board 를 합쳐 오는 것)와 별개다. 여기 항목은
//! **pane 호스트(kasa-serve-web)의 주소**이고, 이사(migrate)의 목적지·아로나
//! 이사 탭의 기계 목록·경로 매핑(roots)이 전부 이 파일 하나를 본다.
//!
//! 설정: `~/.config/kasaterm/machines.json`
//! ```json
//! [{"label":"맥미니","base":"http://127.0.0.1:18791",
//!   "roots":{"/Users/kasa/Desktop/momewomo":"/Users/miku/momewomo"}},
//!  {"label":"나쵸네코","ssh":"nachoneko"}]
//! ```
//! 둘째 꼴이 설정 화면이 적는 것이다(2026-09-07 지시 「ssh 연결이랑 이름 붙이기를
//! 설정에서」): `ssh` 대상만 있으면 앱이 그 기계의 kasaterm(8765)로 가는 터널을
//! 스스로 들고(`tunnel_loop`) `base` 를 그 터널로 잡는다. 화면공유 주소(host)와
//! 경로 매핑(roots: 이쪽 홈 → 저쪽 홈)도 ssh 로 한 번 물어 채운다. `base` 를 손으로
//! 적은 항목(옛 launchd 터널)은 그대로 존중한다.
//! env `KASATERM_MACHINES`(같은 JSON)가 우선 — 검증용 인스턴스가 사용자 설정을
//! 안 건드리고 가짜 원격을 가리키기 위해서다(다른 격리 env 들과 같은 규율).
//!
//! ⚠️ 원격 상태는 요청 시점에 묻지 않는다 — remoteboard 와 같은 이유다. 아로나가
//! 몇 초마다 폴링하는데 기계가 꺼져 있으면 그 폴링마다 타임아웃만큼 응답이 선다.
//! 백그라운드 루프가 미리 받아 두고, `snapshot()` 은 캐시만 즉시 읽는다.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

const POLL_EVERY: Duration = Duration::from_secs(5);
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);
/// 이보다 오래 소식이 없으면 offline 으로 표시한다. 폴링 두 번을 놓쳐도 살아
/// 있게 여유를 둔다(순간 부하로 한 번 늦는 것과 꺼진 것을 가른다).
const STALE_AFTER: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub label: String,
    /// pane 호스트 주소(`http://127.0.0.1:18791`) — /term/* 가 사는 곳.
    pub base: String,
    /// 그 기계의 **진짜** 주소(`user@10.1.2.3` 꼴 허용) — 화면공유(vnc://) 등
    /// HTTP 창구 밖의 문에 쓴다. base 는 대개 SSH 터널(127.0.0.1)이라 못 쓴다.
    /// 명부의 `host` 값, 없으면 base 의 호스트가 루프백이 아닐 때만 유도. 빈값 가능.
    pub host: String,
    /// IP KVM 웹 주소(예: `https://10.1.21.150/kvm/`) — 있으면 「화면 보기」가
    /// 화면공유 대신 이 문을 연다(거노 지시 2026-09-01). KVM 은 OS 밖 물리 콘솔이라
    /// 로그인 전·부팅 화면까지 보인다 — 화면공유는 그 기계 OS 가 살아 있어야 한다.
    pub kvm: Option<String>,
    /// 로컬 경로 → 그 기계 경로. 긴 접두부터 맞춘다 — nacho-neko 처럼 부모와
    /// 다른 자리에 사는 레포를 부모 규칙보다 먼저 잡기 위해서다.
    pub roots: Vec<(String, String)>,
    /// 본진 — 순정 `claude` 가 이 기계 태생으로 간다(셰임의 home 디스패치).
    /// 옵트인이라 기본 false 고, **한 기계에만** 걸어야 한다: 서로가 서로를
    /// 본진으로 걸면 스폰이 두 기계 사이를 무한히 오간다(가드가 없다 — 명부는
    /// 기계마다 따로라 코드가 원천 차단할 수 없다).
    pub home: bool,
    /// ssh 대상(`nachoneko`·`user@10.0.0.5`). 있고 `base` 가 없으면 앱이 터널을 든다.
    pub ssh: Option<String>,
    /// `base` 가 이 앱의 자동 터널(`tunnel_loop`)인가 — 그 항목만 터널을 스폰한다.
    pub tunneled: bool,
}

/// 자동 터널의 로컬 포트 — 라벨에서 결정적으로 뽑는다(파일에 안 적어도 재시작마다
/// 같은 번호). 손으로 만든 launchd 터널(18791·18795…)과 겹치지 않게 18900 대.
pub fn tunnel_port(label: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in label.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    18900 + (h % 90) as u16
}

/// ssh 로 한 번 물어 둔 그 기계의 정체 — 화면공유 주소(hostname)와 홈 폴더.
#[derive(Clone, Default)]
struct RemoteMeta {
    hostname: String,
    home: String,
}
fn meta_cache() -> &'static Mutex<HashMap<String, RemoteMeta>> {
    static C: OnceLock<Mutex<HashMap<String, RemoteMeta>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse(v: &Value) -> Vec<Machine> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|m| {
            let label = m.get("label")?.as_str()?.trim().to_string();
            if label.is_empty() {
                return None;
            }
            let ssh = m
                .get("ssh")
                .and_then(|s| s.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let explicit_base = m
                .get("base")
                .and_then(|b| b.as_str())
                .map(|b| b.trim().trim_end_matches('/').to_string())
                .filter(|b| !b.is_empty());
            let tunneled = explicit_base.is_none() && ssh.is_some();
            let base = match explicit_base {
                Some(b) => b,
                None if ssh.is_some() => format!("http://127.0.0.1:{}", tunnel_port(&label)),
                None => return None,
            };
            let meta = ssh
                .as_ref()
                .and_then(|t| meta_cache().lock().ok()?.get(t).cloned())
                .unwrap_or_default();
            let host = m
                .get("host")
                .and_then(|h| h.as_str())
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
                .or_else(|| (!meta.hostname.is_empty()).then(|| meta.hostname.clone()))
                .unwrap_or_else(|| {
                    let h = base
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .split(['/', ':'])
                        .next()
                        .unwrap_or("");
                    if h == "127.0.0.1" || h == "localhost" {
                        String::new()
                    } else {
                        h.to_string()
                    }
                });
            let mut roots: Vec<(String, String)> = m
                .get("roots")
                .and_then(|r| r.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            // ssh 항목에 규칙이 없으면 「이쪽 홈 → 저쪽 홈」 하나를 기본으로 —
            // ~/Desktop/… 이 저쪽 같은 자리에 앉는다. 저쪽 홈은 tunnel_loop 가 ssh 로
            // 한 번 물어 두며, 아직 못 물었으면 규칙 없이 간다(이사가 그때 「roots 에
            // 규칙을」로 서고, 몇 초 뒤 다시 누르면 된다).
            if roots.is_empty() && ssh.is_some() && !meta.home.is_empty() {
                if let Ok(home) = std::env::var("HOME") {
                    roots.push((home, meta.home.clone()));
                }
            }
            // 긴 접두가 먼저 이겨야 한다 — 정렬을 여기서 굳혀 두면 매핑 함수는
            // 앞에서부터 첫 일치를 집으면 된다.
            roots.sort_by_key(|(l, _)| std::cmp::Reverse(l.len()));
            let kvm = m
                .get("kvm")
                .and_then(|k| k.as_str())
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty());
            let home = m.get("home").and_then(|h| h.as_bool()).unwrap_or(false);
            Some(Machine {
                label,
                base,
                host,
                roots,
                kvm,
                home,
                ssh,
                tunneled,
            })
        })
        .collect()
}

/// 명부 파일 경로. env `KASATERM_MACHINES_FILE` 이 있으면 그 파일(검증용 인스턴스가
/// 설정 화면의 쓰기까지 격리하려고 준다). env `KASATERM_MACHINES`(JSON 본문)만 걸린
/// 인스턴스는 None — 그런 판은 사용자 명부를 읽지도 쓰지도 않는다.
pub fn machines_path() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_MACHINES_FILE") {
        if !p.is_empty() {
            return Some(std::path::PathBuf::from(p));
        }
    }
    if std::env::var("KASATERM_MACHINES").is_ok() {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join(".config/kasaterm/machines.json"))
}

/// 설정 화면용 — 파일의 항목을 **있는 그대로**(모르는 필드 포함) 준다. 화면이
/// 아는 필드(label·ssh)만 고치고 나머지는 되돌려 써야 손으로 적은 roots·kvm 이
/// 안 날아간다.
pub fn entries() -> Vec<Value> {
    let Some(path) = machines_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

/// 설정 화면용 — 항목 전부를 파일에 쓴다(통째 교체). 폴링·터널은 매 바퀴 파일을
/// 다시 읽으므로 재시작 없이 다음 바퀴부터 반영된다.
pub fn save_entries(list: &[Value]) -> std::io::Result<()> {
    let Some(path) = machines_path() else {
        return Err(std::io::Error::other("격리 인스턴스(KASATERM_MACHINES)에선 명부를 안 쓴다"));
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(&Value::Array(list.to_vec()))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

// ── 자동 터널 ────────────────────────────────────────────────────────────

struct Tunnel {
    child: std::process::Child,
    target: String,
    port: u16,
}
fn tunnels() -> &'static Mutex<HashMap<String, Tunnel>> {
    static T: OnceLock<Mutex<HashMap<String, Tunnel>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}
/// 라벨 → 마지막 스폰 시각. 죽자마자 다시 띄우면 안 닿는 기계에 초당 ssh 를 쏜다.
fn last_spawn() -> &'static Mutex<HashMap<String, Instant>> {
    static L: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}
const TUNNEL_TICK: Duration = Duration::from_secs(3);
const TUNNEL_RETRY: Duration = Duration::from_secs(8);
const META_RETRY: Duration = Duration::from_secs(60);

fn ssh_output(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 그 ssh 대상의 hostname(화면공유 주소)과 홈을 한 번 물어 둔다. 실패는 60초에
/// 한 번만 다시 — 안 닿는 기계에 매 바퀴 ssh 를 쏘지 않게.
fn ensure_meta(target: &str) {
    static TRIED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let tried = TRIED.get_or_init(|| Mutex::new(HashMap::new()));
    let known = meta_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(target).cloned())
        .is_some_and(|m| !m.home.is_empty());
    if known {
        return;
    }
    if let Ok(mut t) = tried.lock() {
        if t.get(target).is_some_and(|at| at.elapsed() < META_RETRY) {
            return;
        }
        t.insert(target.to_string(), Instant::now());
    }
    // `ssh -G` 는 접속 없이 설정만 푼다 — alias 뒤의 진짜 주소가 여기서 나온다.
    let hostname = ssh_output(&["-G", target])
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("hostname "))
                .map(|h| h.trim().to_string())
        })
        .unwrap_or_default();
    let home = ssh_output(&[target, "printf %s \"$HOME\""]).unwrap_or_default();
    if let Ok(mut c) = meta_cache().lock() {
        let e = c.entry(target.to_string()).or_default();
        if !hostname.is_empty() {
            e.hostname = hostname;
        }
        if !home.is_empty() {
            e.home = home;
        }
    }
}

fn tunnel_tick() {
    let want: Vec<(String, String, u16)> = machines()
        .into_iter()
        .filter(|m| m.tunneled)
        .filter_map(|m| Some((m.label.clone(), m.ssh.clone()?, tunnel_port(&m.label))))
        .collect();
    let Ok(mut t) = tunnels().lock() else { return };
    // 명부에서 빠졌거나 대상이 바뀐 터널은 걷는다.
    t.retain(|label, tun| {
        let keep = want.iter().any(|(l, tg, p)| l == label && *tg == tun.target && *p == tun.port);
        if !keep {
            let _ = tun.child.kill();
            let _ = tun.child.wait();
            eprintln!("[machines] {label} 터널 걷음(명부에서 빠짐)");
        }
        keep
    });
    for (label, target, port) in want {
        ensure_meta(&target);
        if let Some(tun) = t.get_mut(&label) {
            match tun.child.try_wait() {
                Ok(None) => continue, // 살아 있다
                _ => {
                    eprintln!("[machines] {label} 터널 끊김 — 다시 연다");
                    t.remove(&label);
                }
            }
        }
        if let Ok(mut l) = last_spawn().lock() {
            if l.get(&label).is_some_and(|at| at.elapsed() < TUNNEL_RETRY) {
                continue;
            }
            l.insert(label.clone(), Instant::now());
        }
        let spawned = std::process::Command::new("ssh")
            .args([
                "-N",
                "-o",
                "BatchMode=yes",
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "ServerAliveInterval=20",
                "-o",
                "ServerAliveCountMax=3",
                "-o",
                "ConnectTimeout=8",
                "-L",
                &format!("{port}:127.0.0.1:8765"),
                &target,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                eprintln!("[machines] {label} 터널 염: 127.0.0.1:{port} → {target}:8765");
                t.insert(label, Tunnel { child, target, port });
            }
            Err(e) => eprintln!("[machines] {label} 터널 스폰 실패: {e}"),
        }
    }
}

/// 백그라운드 — `ssh` 만 적힌 기계마다 8765 터널을 들고 있는다. 끊기면 8초 뒤
/// 다시 열고, 명부에서 빠지면 걷는다. 폴링 루프와 같은 이유로 본체 한정.
pub async fn tunnel_loop() {
    loop {
        let _ = tokio::task::spawn_blocking(tunnel_tick).await;
        tokio::time::sleep(TUNNEL_TICK).await;
    }
}

/// 앱을 끌 때 — 자식 ssh 가 고아로 남지 않게.
pub fn stop_tunnels() {
    if let Ok(mut t) = tunnels().lock() {
        for (_, mut tun) in t.drain() {
            let _ = tun.child.kill();
            let _ = tun.child.wait();
        }
    }
}

pub fn machines() -> Vec<Machine> {
    if let Ok(s) = std::env::var("KASATERM_MACHINES") {
        if let Ok(v) = serde_json::from_str::<Value>(&s) {
            let m = parse(&v);
            if !m.is_empty() {
                return m;
            }
        }
    }
    let Some(path) = machines_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| parse(&v))
        .unwrap_or_default()
}

pub fn find(label: &str) -> Option<Machine> {
    machines().into_iter().find(|m| m.label == label)
}

/// 본진(home:true) 기계 — 여럿이면 첫 항목이 이긴다(걸 일이 없어야 하는 상태라
/// 굳이 오류로 만들지 않는다).
pub fn home_machine() -> Option<Machine> {
    machines().into_iter().find(|m| m.home)
}

/// 주소로 라벨 역조회 — surface.remote 처럼 주소만 들고 들어온 링크에 이름을
/// 붙여 준다. 명부 밖 주소면 None.
pub fn label_for_base(base: &str) -> Option<String> {
    let b = base.trim_end_matches('/');
    machines()
        .into_iter()
        .find(|m| m.base == b)
        .map(|m| m.label)
}

/// 경로 접두 매핑. 경계가 path 성분이어야 한다 — `/a/bc` 가 `/a/b` 규칙에
/// 걸리면 엉뚱한 폴더가 된다.
fn map_prefix(path: &str, from: &str, to: &str) -> Option<String> {
    let rest = path.strip_prefix(from)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    Some(format!("{to}{rest}"))
}

pub fn map_local_to_remote(m: &Machine, local: &str) -> Option<String> {
    m.roots.iter().find_map(|(l, r)| map_prefix(local, l, r))
}

pub fn map_remote_to_local(m: &Machine, remote: &str) -> Option<String> {
    // 역방향도 긴 접두 우선 — remote 쪽 길이로 다시 고른다(정렬은 local 기준이라).
    let mut hits: Vec<String> = Vec::new();
    let mut best_len = 0usize;
    for (l, r) in &m.roots {
        if let Some(mapped) = map_prefix(remote, r, l) {
            if r.len() > best_len {
                best_len = r.len();
                hits.clear();
                hits.push(mapped);
            }
        }
    }
    hits.into_iter().next()
}

/// 캐시: 라벨 → (마지막으로 닿은 시각, /term/panes 행들, 싱크 창구 유무).
/// 셋째 값이 false 면 그 기계의 프로그램이 낡아(repo-sync 창구 없음) 변경 실은
/// 이사가 선다 — 이사 탭이 「프로그램 낡음」 경고를 그리는 근거다.
type Cache = HashMap<String, (Instant, Vec<Value>, bool)>;

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 그 기계 프로그램에 repo-sync 창구가 있나 — 낡은 판은 라우트 자체가 없어
/// 404 를 돌려준다(새 판은 인자 오류라도 200 JSON). 판정 불능(타임아웃 등)은
/// 낡음으로 몰지 않는다 — 경고는 확신할 때만.
async fn probe_sync(client: &reqwest::Client, base: &str) -> bool {
    match client
        .get(format!("{base}/term/repo-sync"))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
    {
        Ok(r) => r.status().as_u16() != 404 && r.status().as_u16() != 405,
        Err(_) => true,
    }
}

async fn fetch_panes(client: &reqwest::Client, base: &str) -> Option<Vec<Value>> {
    let resp = client
        .get(format!("{base}/term/panes"))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let text = resp.text().await.ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()?
        .as_array()
        .cloned()
}

/// 백그라운드 폴링. 명부는 **매 바퀴 다시 읽는다** — 부팅 때 한 번만 잡으면
/// machines.json 을 고쳐도 재시작 전까지 옛 주소를 두드린다(2026-08-29 실측:
/// 미니 창구를 옛 서버→본진 앱으로 바꿨는데 폴링만 옛 주소에 남았다).
/// 명부가 비어 있으면 바깥 fetch 는 안 나간다(remoteboard 규율) — 파일 한 번
/// 읽고 자는 것뿐이라 루프 자체는 싸다.
pub async fn poll_loop() {
    let client = reqwest::Client::new();
    let mut announced: Vec<String> = Vec::new();
    loop {
        let list = machines();
        let labels: Vec<String> = list.iter().map(|m| m.label.clone()).collect();
        if labels != announced {
            eprintln!("[machines] {} 곳 폴링: {}", list.len(), labels.join(", "));
            announced = labels;
        }
        for m in &list {
            if let Some(panes) = fetch_panes(&client, &m.base).await {
                let sync = probe_sync(&client, &m.base).await;
                if let Ok(mut c) = cache().lock() {
                    c.insert(m.label.clone(), (Instant::now(), panes, sync));
                }
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// GET /machines 응답 본체. 캐시만 읽으므로 기계가 죽어 있어도 즉시다.
pub fn snapshot() -> Vec<Value> {
    let c = cache().lock().ok();
    machines()
        .into_iter()
        .map(|m| {
            let hit = c.as_ref().and_then(|c| c.get(&m.label));
            let age = hit.map(|(at, _, _)| at.elapsed());
            let online = age.is_some_and(|a| a < STALE_AFTER);
            serde_json::json!({
                "label": m.label,
                "base": m.base,
                "ssh": m.ssh,
                "online": online,
                "ago_secs": age.map(|a| a.as_secs()),
                "sync_capable": hit.map(|(_, _, s)| *s).unwrap_or(true),
                "panes": if online {
                    hit.map(|(_, p, _)| p.clone()).unwrap_or_default()
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_only_entry_gets_a_tunnel_base() {
        let v = parse(&serde_json::json!([{"label": "나쵸네코", "ssh": "nachoneko"}]));
        assert_eq!(v.len(), 1);
        assert!(v[0].tunneled);
        assert_eq!(v[0].ssh.as_deref(), Some("nachoneko"));
        assert_eq!(v[0].base, format!("http://127.0.0.1:{}", tunnel_port("나쵸네코")));
        let p = tunnel_port("나쵸네코");
        assert!((18900..18990).contains(&p));
        assert_eq!(p, tunnel_port("나쵸네코"));
    }

    #[test]
    fn explicit_base_wins_over_ssh() {
        let v = parse(&serde_json::json!([{"label": "미니", "ssh": "mini", "base": "http://127.0.0.1:18795/"}]));
        assert!(!v[0].tunneled);
        assert_eq!(v[0].base, "http://127.0.0.1:18795");
    }

    #[test]
    fn entry_without_base_or_ssh_is_dropped() {
        assert!(parse(&serde_json::json!([{"label": "빈것"}])).is_empty());
    }

    fn m() -> Machine {
        parse(&serde_json::json!([{
            "label": "미니",
            "base": "http://127.0.0.1:18791/",
            "roots": {
                "/Users/kasa/Desktop/momewomo": "/Users/miku/momewomo",
                "/Users/kasa/Desktop/momewomo/nacho-neko": "/Users/miku/nacho-neko",
                "/Users/kasa/Desktop": "/Users/miku/Desktop",
            },
        }]))
        .remove(0)
    }

    #[test]
    fn kvm_field_parses_and_blank_means_none() {
        // kvm 이 있으면 「화면 보기」가 화면공유 대신 이 문을 연다 — 빈 문자열은
        // 없는 것과 같아야 한다(반쪽 설정으로 빈 주소를 열지 않게).
        let v = serde_json::json!([
            {"label":"팜","base":"http://127.0.0.1:1","kvm":"https://10.1.21.150/kvm/"},
            {"label":"빈값","base":"http://127.0.0.1:2","kvm":"  "},
            {"label":"없음","base":"http://127.0.0.1:3"},
        ]);
        let ms = parse(&v);
        assert_eq!(ms[0].kvm.as_deref(), Some("https://10.1.21.150/kvm/"));
        assert_eq!(ms[1].kvm, None);
        assert_eq!(ms[2].kvm, None);
    }

    #[test]
    fn longest_local_prefix_wins() {
        // nacho-neko 는 부모(momewomo) 규칙보다 자기 규칙을 먼저 받아야 한다.
        let m = m();
        assert_eq!(
            map_local_to_remote(&m, "/Users/kasa/Desktop/momewomo/nacho-neko").as_deref(),
            Some("/Users/miku/nacho-neko")
        );
        assert_eq!(
            map_local_to_remote(&m, "/Users/kasa/Desktop/momewomo/tmuxify").as_deref(),
            Some("/Users/miku/momewomo/tmuxify")
        );
        assert_eq!(
            map_local_to_remote(&m, "/Users/kasa/Desktop").as_deref(),
            Some("/Users/miku/Desktop")
        );
    }

    #[test]
    fn prefix_must_end_on_a_path_boundary() {
        // "/Users/kasa/Desktop" 규칙이 "/Users/kasa/Desktop2" 를 물면 안 된다.
        let m = m();
        assert_eq!(map_local_to_remote(&m, "/Users/kasa/Desktop2/x"), None);
    }

    #[test]
    fn reverse_mapping_prefers_the_longest_remote_prefix() {
        let m = m();
        assert_eq!(
            map_remote_to_local(&m, "/Users/miku/nacho-neko").as_deref(),
            Some("/Users/kasa/Desktop/momewomo/nacho-neko")
        );
        assert_eq!(
            map_remote_to_local(&m, "/Users/miku/momewomo/tmuxify").as_deref(),
            Some("/Users/kasa/Desktop/momewomo/tmuxify")
        );
    }

    #[test]
    fn base_trailing_slash_is_normalized() {
        assert_eq!(m().base, "http://127.0.0.1:18791");
    }
}
