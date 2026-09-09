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

use std::collections::{HashMap, HashSet};
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
    /// 공용 관문의 업링크가 쓰는 영구 id. 설정에 직접 있거나 `/version`에서 배운다.
    pub machine_id: Option<String>,
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
    /// 그 기계의 카사크롬 다리(8777)가 **이쪽에서** 닿는 로컬 포트. 손으로 든 터널
    /// (미니에서 본 맥북 = 18800)이면 여기 적고, 없으면 `ssh` 가 있을 때 앱이
    /// `chrome_tunnel_port` 로 포워드를 든다. 「카사크롬이 쓰는 크롬」 설정이 본다.
    pub chrome_port: Option<u16>,
    /// ssh 열쇠 파일. 비어 있으면 기본 열쇠로 가고, 그게 거절되면 `~/.ssh` 의 열쇠를
    /// 하나씩 대 보아 맞는 것을 여기 적어 둔다(`ensure_meta`) — 열쇠 로그인만 받는
    /// 기계(윈도우 sshd)를 별칭 없이 `user@host` 만으로 넣기 위해서(2026-09-07 지시).
    pub key: Option<String>,
    /// `base` 가 이 앱의 자동 터널(`tunnel_loop`)인가 — 그 항목만 터널을 스폰한다.
    pub tunneled: bool,
    /// 명부 파일이 아니라 **상대가 알려 와서** 생긴 항목(`announce_guest`). 그쪽 ssh 가
    /// 여기로 되돌아오는 포트(-R)를 열어 두어 이쪽은 ssh 없이도 그 카사텀에 닿는다.
    /// 알림이 끊기면 저절로 빠지고, 설정 화면에서 고치거나 지울 것이 없다.
    pub guest: bool,
}

/// 되돌아오는 포트 — 상대 기계에서 이쪽 카사텀(8765)으로 오는 `-R` 의 번호.
/// 이쪽 이름에서 결정적으로 뽑아 재시작·고아 ssh 재사용 뒤에도 같은 번호를 알린다
/// (`-R 0:` 으로 받아 오면 stderr 를 읽어야 하고, 고아를 그대로 쓰는 길에선 번호를
/// 잃는다). 앞쪽 터널(18900 대)과 겹치지 않게 19000 대.
pub fn reverse_port(self_label: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in self_label.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    19000 + (h % 500) as u16
}

/// 이 기계가 남에게 알릴 이름 — 컴퓨터 이름(설정 > 공유의 그 이름). 검증용 인스턴스는
/// `KASATERM_SELF_LABEL` 로 바꿔 단다(두 리그가 같은 이름으로 서로 알리면 한 항목이 된다).
pub fn self_label() -> String {
    static L: OnceLock<String> = OnceLock::new();
    L.get_or_init(|| {
        if let Ok(v) = std::env::var("KASATERM_SELF_LABEL") {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
        let by_scutil = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        by_scutil
            .or_else(|| {
                std::process::Command::new("hostname")
                    .arg("-s")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "이 기계".to_string())
    })
    .clone()
}

/// 지금 도는 판의 빌드 표식. 앱이 부팅 때 git 리비전(`KASATERM_GIT_REV`)을 넣어 준다 —
/// kasa-mcp 는 자기 build.rs 가 없고, 앱과 같은 워크스페이스 커밋에서 구워지므로 앱의
/// 것이 곧 이 크레이트의 것이다. 안 넣었으면 크레이트 버전으로 물러선다.
pub fn set_build_id(id: &str) {
    let _ = build_slot().set(id.trim().to_string());
}
pub fn build_id() -> String {
    build_slot()
        .get()
        .cloned()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}
fn build_slot() -> &'static OnceLock<String> {
    static B: OnceLock<String> = OnceLock::new();
    &B
}

/// 상대가 알려 온 기계. `at` 이 GUEST_STALE 을 넘으면 목록에서 빠진다 — 알림은 상대의
/// 폴링(5초)마다 오므로, 터널이 죽으면 반 분 안에 사라진다.
#[derive(Clone)]
struct Guest {
    base: String,
    host: String,
    home: String,
    build: String,
    at: Instant,
}
const GUEST_STALE: Duration = Duration::from_secs(30);
fn guests() -> &'static Mutex<HashMap<String, Guest>> {
    static G: OnceLock<Mutex<HashMap<String, Guest>>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `POST /machines/announce` 의 본체 — 상대가 「나는 <label>, 이 포트로 오면 된다」고
/// 알려 온다. 포트는 상대 ssh 가 이 기계에 열어 둔 `-R` 이라 루프백으로 닿는다.
pub fn announce_guest(label: &str, port: u16, host: &str, home: &str, build: &str) {
    let label = label.trim();
    if label.is_empty() || port == 0 {
        return;
    }
    if let Ok(mut g) = guests().lock() {
        let fresh = !g.contains_key(label);
        g.insert(
            label.to_string(),
            Guest {
                base: format!("http://127.0.0.1:{port}"),
                host: host.trim().to_string(),
                home: home.trim().to_string(),
                build: build.trim().to_string(),
                at: Instant::now(),
            },
        );
        if fresh {
            eprintln!("[machines] {label} 이(가) 알려 옴: 127.0.0.1:{port} (빌드 {build})");
        }
    }
}

/// 알려 온 기계들을 명부 항목으로. 파일 명부와 이름이 겹치면 파일 쪽이 이긴다 —
/// 손으로 적은 ssh·roots 가 더 많은 것을 안다.
fn guest_machines(taken: &[String]) -> Vec<Machine> {
    let Ok(mut g) = guests().lock() else { return Vec::new() };
    g.retain(|_, v| v.at.elapsed() < GUEST_STALE);
    let my_home = std::env::var("HOME").unwrap_or_default();
    g.iter()
        .filter(|(label, _)| !taken.contains(label))
        .map(|(label, v)| Machine {
            label: label.clone(),
            machine_id: None,
            base: v.base.clone(),
            host: v.host.clone(),
            kvm: None,
            roots: if my_home.is_empty() || v.home.is_empty() {
                Vec::new()
            } else {
                vec![(my_home.clone(), v.home.clone())]
            },
            home: false,
            ssh: None,
            key: None,
            tunneled: false,
            chrome_port: None,
            guest: true,
        })
        .collect()
}

/// 알려 온 기계의 빌드 표식(알림에 실려 온 것) — 폴링이 `/version` 을 못 물었을 때의 폴백.
fn guest_build(label: &str) -> Option<String> {
    guests().lock().ok()?.get(label).map(|g| g.build.clone())
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

/// 그 기계 카사크롬 다리(8777)로 가는 앱 포워드의 로컬 포트 — `tunnel_port` 와 같은
/// 규칙, 다른 대역(19600). 명부에 `chrome_port` 를 적으면 그것이 이긴다.
pub fn chrome_tunnel_port(label: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in label.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    19600 + (h % 90) as u16
}

/// 이 기계의 카사크롬 다리 포트(확장 ↔ 브리지, kasachrome/extension/port.js 와 같다).
pub const KASACHROME_PORT: u16 = 8777;

/// 설정 「카사크롬이 쓰는 크롬」 — 명부의 기계 라벨, 빈 문자열이면 이 기계.
pub fn kasachrome_machine() -> String {
    crate::character::read_setting_str("kasachrome_machine")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// 고른 기계의 크롬 다리가 이쪽에서 닿는 로컬 포트. 명부에 없거나 갈 길이 없으면 None.
/// `chrome_port` 만 적힌 항목(base·ssh 없음 — 미니에서 본 맥북처럼 손 터널 `-R` 로만
/// 닿는 기계)은 `parse` 가 기계로 안 세우므로 원문 명부에서 따로 찾는다.
pub fn kasachrome_target_port(label: &str) -> Option<u16> {
    if let Some(m) = find(label) {
        return m
            .chrome_port
            .or_else(|| m.ssh.as_ref().map(|_| chrome_tunnel_port(&m.label)));
    }
    chrome_only_entries()
        .into_iter()
        .find(|(l, _)| l == label)
        .map(|(_, p)| p)
}

/// 명부에서 `chrome_port` 만 있는 항목 — (라벨, 포트).
fn chrome_only_entries() -> Vec<(String, u16)> {
    entries()
        .iter()
        .filter(|e| e.get("base").and_then(|v| v.as_str()).is_none_or(str::is_empty))
        .filter(|e| e.get("ssh").and_then(|v| v.as_str()).is_none_or(str::is_empty))
        .filter_map(|e| {
            let label = e.get("label")?.as_str()?.trim().to_string();
            let port = e.get("chrome_port")?.as_u64()? as u16;
            (!label.is_empty()).then_some((label, port))
        })
        .collect()
}

/// 「카사크롬이 쓰는 크롬」 후보 — 다리로 갈 길이 있는 기계(ssh 나 chrome_port)만.
pub fn kasachrome_candidates() -> Vec<String> {
    let mut out: Vec<String> = listed_machines()
        .into_iter()
        .filter(|m| m.ssh.is_some() || m.chrome_port.is_some())
        .map(|m| m.label)
        .collect();
    for (label, _) in chrome_only_entries() {
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

/// Explicit selection never falls back to a different computer.
pub fn kasachrome_bridge_urls() -> Vec<String> {
    kasachrome_bridge_urls_for(&kasachrome_machine())
}

pub fn kasachrome_bridge_urls_for(chosen: &str) -> Vec<String> {
    if chosen.is_empty() {
        vec![format!("ws://127.0.0.1:{KASACHROME_PORT}")]
    } else {
        kasachrome_target_port(chosen)
            .map(|port| vec![format!("ws://127.0.0.1:{port}")])
            .unwrap_or_default()
    }
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
            let chrome_port = m.get("chrome_port").and_then(|v| v.as_u64()).map(|n| n as u16);
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
            let key = m
                .get("key")
                .and_then(|k| k.as_str())
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty());
            let machine_id = m
                .get("machine_id")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| {
                    (8..=128).contains(&value.len())
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
                })
                .map(str::to_string);
            Some(Machine {
                label,
                machine_id,
                base,
                host,
                roots,
                kvm,
                home,
                ssh,
                key,
                tunneled,
                chrome_port,
                guest: false,
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
    ssh_run(args).ok()
}

/// ssh 한 번 — 성공이면 stdout, 실패면 stderr(거절 이유). 열쇠를 찾을 때 「거절」과
/// 「안 닿음」을 갈라야 해서 이유를 돌려준다.
fn ssh_run(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8"])
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// 열쇠를 지정하는 ssh 인자. `IdentitiesOnly` 가 없으면 ssh 가 기본 열쇠들을 먼저
/// 대 보다 서버 쪽 시도 한도에 걸려 정작 맞는 열쇠 차례가 안 온다.
fn key_args(key: Option<&str>) -> Vec<String> {
    match key {
        Some(k) if !k.is_empty() => vec![
            "-i".to_string(),
            k.to_string(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
        ],
        _ => Vec::new(),
    }
}

/// `~/.ssh` 의 개인 열쇠 후보 — `.pub` 짝이 있는 파일만(config·known_hosts 는 빠진다).
fn candidate_keys() -> Vec<String> {
    let Ok(home) = std::env::var("HOME") else { return Vec::new() };
    let dir = std::path::Path::new(&home).join(".ssh");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut keys: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pub"))
        .filter_map(|p| {
            let private = p.with_extension("");
            private.is_file().then(|| private.to_string_lossy().to_string())
        })
        .collect();
    keys.sort();
    keys
}

/// 찾아낸 열쇠를 명부에 적는다 — 다음 터널·다음 실행이 그걸로 간다.
fn remember_key(target: &str, key: &str) {
    let mut list = entries();
    let mut changed = false;
    for e in list.iter_mut() {
        if e.get("ssh").and_then(|v| v.as_str()) == Some(target) {
            if let Some(o) = e.as_object_mut() {
                o.insert("key".into(), Value::String(key.to_string()));
                changed = true;
            }
        }
    }
    if changed {
        if let Err(e) = save_entries(&list) {
            eprintln!("[machines] {target} 열쇠를 명부에 못 적음: {e}");
        }
    }
}

/// 그 ssh 대상의 hostname(화면공유 주소)과 홈을 한 번 물어 둔다. 실패는 60초에
/// 한 번만 다시 — 안 닿는 기계에 매 바퀴 ssh 를 쏘지 않게.
fn ensure_meta(target: &str, key: Option<&str>) {
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
    let run = |kargs: &[String], cmd: &str| {
        let mut args: Vec<&str> = kargs.iter().map(String::as_str).collect();
        args.extend([target, cmd]);
        ssh_run(&args)
    };
    // 로그인 확인은 `exit 0` — 어느 셸(sh·cmd·PowerShell)에서나 성공한다. 홈을
    // 묻는 명령으로 확인하면 윈도우 sshd(cmd)에선 열쇠가 맞아도 `printf` 가 없어
    // 실패로 보여 열쇠를 영영 못 찾았다(2026-09-07 실측).
    let mut kargs = key_args(key);
    let logged_in = match run(&kargs, "exit 0") {
        Ok(_) => true,
        // 기본 열쇠가 거절됐다 — `~/.ssh` 의 열쇠를 하나씩 대 본다. 안 닿는 기계엔
        // 안 한다(열쇠마다 8초 타임아웃이 쌓인다).
        Err(why) if key.is_none() && why.contains("Permission denied") => {
            let mut ok = false;
            for k in candidate_keys() {
                let ka = key_args(Some(&k));
                if run(&ka, "exit 0").is_ok() {
                    eprintln!("[machines] {target} 열쇠 찾음: {k}");
                    remember_key(target, &k);
                    kargs = ka;
                    ok = true;
                    break;
                }
            }
            ok
        }
        Err(_) => false,
    };
    // 홈 — sh 가 먼저, 없으면 cmd(윈도우). 둘 다 안 되면 빈값(roots 규칙 없이 간다).
    let home = if logged_in {
        run(&kargs, "printf %s \"$HOME\"")
            .ok()
            .filter(|h| !h.is_empty())
            .or_else(|| run(&kargs, "echo %USERPROFILE%").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty() && !h.contains("%USERPROFILE%"))
            .unwrap_or_default()
    } else {
        String::new()
    };
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

/// 감시꾼(sh)을 걷는다 — **TERM 으로**. `Child::kill` 은 SIGKILL 이라 trap 이 못 돌아
/// 밑의 ssh 가 고아로 남는다(2026-09-09 실측: 앱을 곱게 끝냈는데 크롬 포워드 ssh 가
/// 살아 있었다). TERM 뒤 잠깐 기다리고, 그래도 남으면 그때 KILL.
fn stop_watched(tun: &mut Tunnel) {
    let pid = tun.child.id();
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    for _ in 0..40 {
        if let Ok(Some(_)) = tun.child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = tun.child.kill();
    let _ = tun.child.wait();
}

/// ssh 를 sh 감시꾼 밑에 띄운다 — 앱이 SIGTERM·크래시로 죽으면 `exiting`
/// (stop_tunnels)이 안 돌아 ssh 가 고아로 남는다(2026-09-07 실측: 격리 앱을
/// kill 하니 18945 터널이 그대로 살아 있었다). macOS 엔 부모 죽음 신호가 없어
/// 감시꾼이 앱 pid($PPID)를 3초마다 보고 없어지면 ssh 를 걷는다. 감시꾼 자신이
/// TERM 을 받아도(stop_tunnels) trap 이 ssh 를 같이 걷는다. `forwards` 는
/// `-L`/`-R` 인자 그대로.
fn spawn_watched_ssh(
    kargs: &[String],
    forwards: &[String],
    target: &str,
) -> std::io::Result<std::process::Child> {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(
            // `sleep 3 & wait $!` — 그냥 `sleep 3` 이면 TERM 이 와도 sleep 이 끝나야
            // trap 이 돌아, 3초 안에 KILL 폴백이 먼저 오면 ssh 가 고아로 남는다.
            "p=\"\"; trap 'kill $p 2>/dev/null; exit 0' TERM INT\n\
ssh \"$@\" & p=$!\n\
while kill -0 $PPID 2>/dev/null && kill -0 $p 2>/dev/null; do sleep 3 & wait $!; done\n\
kill $p 2>/dev/null; wait $p 2>/dev/null",
        )
        .arg("kasaterm-tunnel")
        .arg("-N")
        .args(kargs)
        .args([
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
        ])
        .args(forwards)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// 「카사크롬이 쓰는 크롬」이 다른 기계면 그 기계의 다리(8777)로 가는 포워드를 든다.
/// 명부에 `chrome_port` 가 적힌 기계(손으로 든 터널)나 `ssh` 없는 기계는 앱이 들
/// 것이 없다. 고른 기계가 바뀌면 옛 포워드는 걷는다.
fn chrome_tunnel_tick() {
    let chosen = kasachrome_machine();
    let want = find(&chosen).filter(|m| m.chrome_port.is_none()).and_then(|m| {
        Some((m.label.clone(), m.ssh.clone()?, chrome_tunnel_port(&m.label), m.key.clone()))
    });
    let Ok(mut t) = chrome_tunnels().lock() else { return };
    t.retain(|label, tun| {
        let keep = want
            .as_ref()
            .is_some_and(|(l, tg, p, _)| l == label && *tg == tun.target && *p == tun.port);
        if !keep {
            stop_watched(tun);
            eprintln!("[machines] {label} 크롬 포워드 걷음(설정이 바뀜)");
        }
        keep
    });
    let Some((label, target, port, key)) = want else { return };
    // 열쇠는 8765 터널과 같은 길 — 기본 열쇠가 거절되면 ~/.ssh 에서 맞는 것을 찾아
    // 명부에 적어 둔다(base 가 손 터널이라 8765 터널이 안 도는 기계는 여기서 처음 찾는다).
    ensure_meta(&target, key.as_deref());
    let key = key.or_else(|| {
        entries()
            .iter()
            .find(|e| e.get("ssh").and_then(|v| v.as_str()) == Some(target.as_str()))
            .and_then(|e| e.get("key").and_then(|v| v.as_str()).map(str::to_string))
    });
    let kargs = key_args(key.as_deref());
    if let Some(tun) = t.get_mut(&label) {
        match tun.child.try_wait() {
            Ok(None) => return,
            _ => {
                eprintln!("[machines] {label} 크롬 포워드 끊김 — 다시 연다");
                t.remove(&label);
            }
        }
    }
    let retry_key = format!("chrome:{label}");
    if let Ok(mut l) = last_spawn().lock() {
        if l.get(&retry_key).is_some_and(|at| at.elapsed() < TUNNEL_RETRY) {
            return;
        }
        l.insert(retry_key, Instant::now());
    }
    if std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
    {
        return;
    }
    let forwards = vec!["-L".to_string(), format!("{port}:127.0.0.1:{KASACHROME_PORT}")];
    match spawn_watched_ssh(&kargs, &forwards, &target) {
        Ok(child) => {
            eprintln!("[machines] {label} 크롬 포워드 염: 127.0.0.1:{port} → {target}:{KASACHROME_PORT}");
            t.insert(label, Tunnel { child, target, port });
        }
        Err(e) => eprintln!("[machines] {label} 크롬 포워드 스폰 실패: {e}"),
    }
}

fn chrome_tunnels() -> &'static Mutex<HashMap<String, Tunnel>> {
    static T: OnceLock<Mutex<HashMap<String, Tunnel>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn tunnel_tick() {
    let want: Vec<(String, String, u16, Option<String>)> = listed_machines()
        .into_iter()
        .filter(|m| m.tunneled)
        .filter_map(|m| Some((m.label.clone(), m.ssh.clone()?, tunnel_port(&m.label), m.key.clone())))
        .collect();
    let Ok(mut t) = tunnels().lock() else { return };
    // 명부에서 빠졌거나 대상이 바뀐 터널은 걷는다.
    t.retain(|label, tun| {
        let keep = want.iter().any(|(l, tg, p, _)| l == label && *tg == tun.target && *p == tun.port);
        if !keep {
            stop_watched(tun);
            eprintln!("[machines] {label} 터널 걷음(명부에서 빠짐)");
        }
        keep
    });
    for (label, target, port, key) in want {
        ensure_meta(&target, key.as_deref());
        // 열쇠는 ensure_meta 가 방금 찾아 적었을 수 있다 — 파일에서 다시 읽는다.
        let key = key.or_else(|| {
            entries()
                .iter()
                .find(|e| e.get("ssh").and_then(|v| v.as_str()) == Some(target.as_str()))
                .and_then(|e| e.get("key").and_then(|v| v.as_str()).map(str::to_string))
        });
        let kargs = key_args(key.as_deref());
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
        // 그 포트를 이미 누가 듣고 있으면(앱이 죽으며 남긴 고아 ssh, 또는 같은 명부를
        // 든 다른 인스턴스) 그걸 그냥 쓴다 — ExitOnForwardFailure 로 새 ssh 는 곧장
        // 죽어 8초마다 헛스폰만 돌고, base 는 그 고아가 이미 살리고 있다.
        if std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            Duration::from_millis(300),
        )
        .is_ok()
        {
            continue;
        }
        let mut forwards = vec!["-L".to_string(), format!("{port}:127.0.0.1:8765")];
        // 되돌아오는 길도 같이 연다 — 저쪽에서 이쪽 카사텀으로 오는 `-R`. 이 포트를
        // 폴링이 `/machines/announce` 로 알려 주면 저쪽 명부에 손을 안 대도 그쪽
        // `to` 에 이 기계가 뜬다(2026-09-07 지시 「안 넣어도 양방향」). 이쪽 MCP
        // 포트를 아직 모르면(정본 포트를 못 잡은 검증 인스턴스 등) 앞쪽만 연다.
        if let Some(lp) = local_mcp_port() {
            forwards.push("-R".to_string());
            forwards.push(format!("{}:127.0.0.1:{lp}", reverse_port(&self_label())));
        }
        let spawned = spawn_watched_ssh(&kargs, &forwards, &target);
        match spawned {
            Ok(child) => {
                eprintln!(
                    "[machines] {label} 터널 염: 127.0.0.1:{port} → {target}:8765{}",
                    local_mcp_port()
                        .map(|lp| format!(" · 되돌아옴 {}→{lp}", reverse_port(&self_label())))
                        .unwrap_or_default()
                );
                t.insert(label, Tunnel { child, target, port });
            }
            Err(e) => eprintln!("[machines] {label} 터널 스폰 실패: {e}"),
        }
    }
}

/// 이쪽 카사텀의 MCP 포트 — 앱이 서버를 띄우며 env 에 적는다(session.rs).
fn local_mcp_port() -> Option<u16> {
    std::env::var("KASASPACE_MCP_PORT").ok()?.parse().ok()
}

/// 백그라운드 — `ssh` 만 적힌 기계마다 8765 터널을 들고 있는다. 끊기면 8초 뒤
/// 다시 열고, 명부에서 빠지면 걷는다. 폴링 루프와 같은 이유로 본체 한정.
pub async fn tunnel_loop() {
    loop {
        let _ = tokio::task::spawn_blocking(tunnel_tick).await;
        let _ = tokio::task::spawn_blocking(chrome_tunnel_tick).await;
        tokio::time::sleep(TUNNEL_TICK).await;
    }
}

/// 앱을 끌 때 — 자식 ssh 가 고아로 남지 않게.
pub fn stop_tunnels() {
    for map in [tunnels(), chrome_tunnels()] {
        if let Ok(mut t) = map.lock() {
            for (_, mut tun) in t.drain() {
                stop_watched(&mut tun);
            }
        }
    }
}

fn raw_machines() -> Vec<Machine> {
    let mut list = listed_machines();
    let taken: Vec<String> = list.iter().map(|m| m.label.clone()).collect();
    list.extend(guest_machines(&taken));
    list
}

/// Keep both transport routes alive, but expose one device per confirmed ID.
/// Names, localhost ports and host aliases are not proof of machine identity.
pub fn machines() -> Vec<Machine> {
    let list = raw_machines();
    let Ok(c) = cache().lock() else { return list };
    canonical_roster(list, &c)
}

fn canonical_roster(list: Vec<Machine>, c: &Cache) -> Vec<Machine> {
    let mut out: Vec<Machine> = Vec::new();
    for mut machine in list {
        machine.machine_id = machine.machine_id.or_else(|| c.get(&machine.label)?.machine_id.clone());
        let duplicate = machine.machine_id.as_ref().and_then(|id|
            out.iter().position(|other| other.machine_id.as_ref() == Some(id)));
        let Some(index) = duplicate else { out.push(machine); continue };
        let existing = &out[index];
        let online = |m: &Machine| c.get(&m.label).is_some_and(|s| s.at.elapsed() < STALE_AFTER);
        // Preserve the user's label, home flag, path rules and SSH settings.
        // If that route is down, the confirmed live reverse route can carry it.
        let (mut preferred, alternate) = if existing.guest && !machine.guest {
            (machine, existing.clone())
        } else { (existing.clone(), machine) };
        if !online(&preferred) && online(&alternate) { preferred.base = alternate.base; }
        out[index] = preferred;
    }
    out
}

fn seen_for_machine<'a>(machine: &Machine, c: &'a Cache) -> Option<&'a Seen> {
    let own = c.get(&machine.label);
    if own.is_some_and(|seen| seen.at.elapsed() < STALE_AFTER) { return own; }
    let id = machine.machine_id.as_deref().or_else(|| own?.machine_id.as_deref());
    id.and_then(|id| c.values().filter(|seen| seen.machine_id.as_deref() == Some(id))
        .max_by_key(|seen| seen.at)).or(own)
}

/// 명부 파일(또는 env)의 항목만 — 알려 온 기계는 뺀다. 터널 스폰이 이걸 본다:
/// 알려 온 기계로는 이쪽이 터널을 들 필요가 없다(그쪽이 이미 들고 있다).
pub fn listed_machines() -> Vec<Machine> {
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
        // Saved mirror links may still use the auto-discovered alias.
        .or_else(|| raw_machines().into_iter().find(|m| m.label == label))
}

/// 폰의 새 경로는 `~<stable id>`, 옛 링크는 표시 label. stable id는 설정값이나
/// direct `/version`에서 배운 값이 정확히 한 기계와 맞을 때만 쓴다.
pub fn find_route(route: &str) -> Option<Machine> {
    let Some(id) = route.strip_prefix('~') else {
        return find(route);
    };
    let list = machines();
    let cached = cache().lock().ok();
    let mut matches = list.into_iter().filter(|machine| {
        machine.machine_id.as_deref() == Some(id)
            || cached
                .as_ref()
                .and_then(|cache| cache.get(&machine.label))
                .and_then(|seen| seen.machine_id.as_deref())
                == Some(id)
    });
    let found = matches.next()?;
    matches.next().is_none().then_some(found)
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
    let raw = raw_machines();
    let target = raw.iter().find(|m| m.base == b)?;
    let c = cache().lock().ok()?;
    let id = target.machine_id.as_deref().or_else(|| c.get(&target.label)?.machine_id.as_deref());
    let canonical = canonical_roster(raw.clone(), &c);
    canonical.iter().find(|m| m.base == b || id.is_some_and(|id| m.machine_id.as_deref() == Some(id)))
        .map(|m| m.label.clone()).or_else(|| Some(target.label.clone()))
}

pub fn same_machine_bases(a: &str, b: &str) -> bool {
    if a.trim_end_matches('/') == b.trim_end_matches('/') { return true; }
    let list = raw_machines();
    let Ok(c) = cache().lock() else { return false };
    let id = |base: &str| list.iter().find(|m| m.base == base.trim_end_matches('/'))
        .and_then(|m| m.machine_id.as_deref().or_else(|| c.get(&m.label)?.machine_id.as_deref()));
    matches!((id(a), id(b)), (Some(a), Some(b)) if a == b)
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

/// 캐시: 라벨 → 마지막으로 닿은 시각, /term/panes 행들, 싱크 창구 유무, 그쪽 빌드.
/// `sync` 가 false 면 그 기계의 프로그램이 낡아(repo-sync 창구 없음) 변경 실은
/// 이사가 선다 — 이사 탭이 「프로그램 낡음」 경고를 그리는 근거다. `build` 는
/// `/version` 응답(없는 옛 판이면 None) — 이쪽과 다르면 기계 탭·`to` 가 경고한다.
#[derive(Clone)]
struct Seen {
    at: Instant,
    panes: Vec<Value>,
    sync: bool,
    build: Option<String>,
    machine_id: Option<String>,
}
type Cache = HashMap<String, Seen>;

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

/// 그 기계 프로그램의 빌드 표식. 옛 판은 라우트가 없어 None — 「모름」도 경고 대상이다
/// (같다고 확인된 것만 조용하다).
#[derive(Default)]
struct VersionInfo {
    build: Option<String>,
    machine_id: Option<String>,
}

async fn fetch_version(client: &reqwest::Client, base: &str) -> Option<VersionInfo> {
    let resp = client
        .get(format!("{base}/version"))
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: Value = serde_json::from_str(&resp.text().await.ok()?).ok()?;
    Some(VersionInfo {
        build: v.get("build").and_then(|value| value.as_str()).map(str::to_string),
        machine_id: v
            .get("machine_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| {
                (8..=128).contains(&value.len())
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
            .map(str::to_string),
    })
}

/// 「나는 여기 있다」 — 이쪽이 터널을 든 기계에 이쪽 이름·되돌아오는 포트·빌드를
/// 알린다. 매 폴링마다 보내는 것이 곧 살아 있다는 신호다(저쪽은 반 분 못 받으면 뺀다).
async fn announce_to(client: &reqwest::Client, base: &str) {
    let Some(_) = local_mcp_port() else { return };
    let body = serde_json::json!({
        "label": self_label(),
        "port": reverse_port(&self_label()),
        "host": std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default(),
        "home": std::env::var("HOME").unwrap_or_default(),
        "build": build_id(),
    });
    let _ = client
        .post(format!("{base}/machines/announce"))
        .timeout(FETCH_TIMEOUT)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await;
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
        let list = raw_machines();
        let labels: Vec<String> = list.iter().map(|m| m.label.clone()).collect();
        if labels != announced {
            eprintln!("[machines] {} 곳 폴링: {}", list.len(), labels.join(", "));
            announced = labels;
        }
        for m in &list {
            if let Some(panes) = fetch_panes(&client, &m.base).await {
                let sync = probe_sync(&client, &m.base).await;
                let version = fetch_version(&client, &m.base).await.unwrap_or_default();
                let build = version.build.or_else(|| guest_build(&m.label));
                if let Ok(mut c) = cache().lock() {
                    c.insert(
                        m.label.clone(),
                        Seen {
                            at: Instant::now(),
                            panes,
                            sync,
                            build,
                            machine_id: version.machine_id,
                        },
                    );
                }
                if m.tunneled {
                    announce_to(&client, &m.base).await;
                }
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// 그 기계의 `/term/panes` 행 하나(캐시). 거울 pane 은 몸통이 저쪽이라 이쪽 board 에
/// 줄이 없다 — 폰 목록이 거울을 「셸」로 그렸다(2026-09-08 지적 「푸리나가 그냥 셸이라고
/// 떠, 미니 미러링된 건데」). 폴링 캐시라 기계가 방금 죽었어도 마지막 모습이 남는다.
pub fn cached_pane(label: &str, pane: &str) -> Option<Value> {
    let c = cache().lock().ok()?;
    let own = c.get(label)?;
    let seen = if own.at.elapsed() < STALE_AFTER { own } else {
        own.machine_id.as_ref().and_then(|id| c.values()
            .filter(|s| s.machine_id.as_ref() == Some(id)).max_by_key(|s| s.at)).unwrap_or(own)
    };
    seen
        .panes
        .iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(pane))
        .cloned()
}

/// GET /machines 응답 본체. 캐시만 읽으므로 기계가 죽어 있어도 즉시다.
pub fn snapshot() -> Vec<Value> {
    snapshot_with_uplinks(&[])
}

fn availability(age: Option<Duration>, uplink: bool) -> (bool, Option<&'static str>) {
    if age.is_some_and(|value| value < STALE_AFTER) {
        (true, Some("direct"))
    } else if uplink {
        (true, Some("uplink"))
    } else {
        (false, None)
    }
}

fn known_aliases(m: &Machine) -> HashSet<String> {
    let mut aliases = HashSet::from([m.label.clone()]);
    for value in [m.ssh.as_deref(), Some(m.host.as_str())]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        aliases.insert(value.to_string());
        let host = value.rsplit_once('@').map(|(_, host)| host).unwrap_or(value);
        aliases.insert(host.split(':').next().unwrap_or(host).to_string());
    }
    aliases
}

fn matching_uplink<'a>(
    m: &Machine,
    hit: Option<&Seen>,
    uplinks: &'a [crate::uplink::GatewayMachine],
) -> Option<&'a crate::uplink::GatewayMachine> {
    if let Some(id) = m
        .machine_id
        .as_deref()
        .or_else(|| hit.and_then(|seen| seen.machine_id.as_deref()))
    {
        return uplinks.iter().find(|uplink| uplink.id == id);
    }
    let aliases = known_aliases(m);
    let matches: Vec<&crate::uplink::GatewayMachine> = uplinks
        .iter()
        .filter(|uplink| {
            aliases.contains(&uplink.machine)
                || uplink.aliases.iter().any(|alias| aliases.contains(alias))
        })
        .collect();
    let last = matches.last()?;
    matches.iter().all(|uplink| uplink.id == last.id).then_some(*last)
}

fn snapshot_machine(
    m: Machine,
    hit: Option<&Seen>,
    uplink: Option<&crate::uplink::GatewayMachine>,
    local_build: &str,
) -> Value {
    let age = hit.map(|seen| seen.at.elapsed());
    let direct_online = age.is_some_and(|value| value < STALE_AFTER);
    let (online, via) = availability(age, uplink.is_some());
    // 업링크 생존은 그 기계에 닿는다는 증거지만 판 번호까지 말해 주진 않는다.
    // 직통이 stale이면 예전에 읽은 build를 현재 값처럼 되살리지 않는다.
    let build = direct_online
        .then(|| hit.and_then(|seen| seen.build.clone()))
        .flatten();
    let route_id = uplink
        .map(|value| value.id.as_str())
        .or(m.machine_id.as_deref())
        .or_else(|| hit.and_then(|seen| seen.machine_id.as_deref()));
    let route = route_id.map_or_else(|| m.label.clone(), |id| format!("~{id}"));
    serde_json::json!({
        "label": m.label,
        "route": route,
        "base": m.base,
        "ssh": m.ssh,
        "guest": m.guest,
        "online": online,
        "online_via": via,
        "ago_secs": age.map(|value| value.as_secs()),
        "sync_capable": hit.map(|seen| seen.sync).unwrap_or(true),
        // 빌드 대조 — 같다고 확인된 것만 true. 모르는 것(옛 판·아직 못 물음)은
        // false 로 두어 경고가 서게 한다. 오프라인이면 물을 게 없어 true.
        "build": build,
        "build_match": !online || build.as_deref() == Some(local_build),
        "panes": if direct_online {
            hit.map(|seen| seen.panes.clone()).unwrap_or_default()
        } else {
            Vec::new()
        },
    })
}

/// 공용 주소 관문이 같은 slug/key 범위에서 확인한 살아 있는 업링크까지 합친 스냅샷.
/// 이름 목록은 `uplink::verified_machines`를 통과한 요청에서만 들어온다.
pub(crate) fn snapshot_with_uplinks(uplinks: &[crate::uplink::GatewayMachine]) -> Vec<Value> {
    let list = machines();
    let c = cache().lock().ok();
    let local_build = build_id();
    list
        .into_iter()
        .map(|m| {
            let hit = c.as_ref().and_then(|c| seen_for_machine(&m, c));
            let uplink = matching_uplink(&m, hit, uplinks);
            snapshot_machine(m, hit, uplink, &local_build)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_device_id_merges_guest_routes_preserving_user_metadata_and_fallback() {
        let configured = parse(&serde_json::json!([{
            "label":"Saved Mini", "ssh":"mini", "base":"http://127.0.0.1:18795", "home":true,
            "roots":{"/local":"/remote"}
        }])).remove(0);
        let mut guest = configured.clone();
        guest.label = "mini.local".into(); guest.guest = true; guest.ssh = None;
        guest.base = "http://127.0.0.1:19011".into(); guest.home = false; guest.roots.clear();
        let seen = |age| Seen { at: Instant::now() - age, panes: vec![], sync:true,
            build:Some("build".into()), machine_id:Some("same-stable-machine-id".into()) };
        let mut c = Cache::from([(configured.label.clone(), seen(Duration::ZERO)),
            (guest.label.clone(), seen(Duration::ZERO))]);
        for rows in [vec![configured.clone(), guest.clone()], vec![guest.clone(), configured.clone()]] {
            let merged = canonical_roster(rows, &c);
            assert_eq!(merged.len(), 1);
            assert_eq!(merged[0].label, configured.label);
            assert_eq!(merged[0].base, configured.base);
            assert_eq!(merged[0].roots, configured.roots);
            assert!(merged[0].home);
        }
        c.get_mut(&configured.label).unwrap().at = Instant::now() - STALE_AFTER;
        let merged = canonical_roster(vec![configured.clone(), guest.clone()], &c);
        assert_eq!(merged[0].label, configured.label);
        assert_eq!(merged[0].base, guest.base);
        assert!(seen_for_machine(&merged[0], &c).unwrap().at.elapsed() < STALE_AFTER);
        c.get_mut(&guest.label).unwrap().machine_id = Some("another-device".into());
        assert_eq!(canonical_roster(vec![configured.clone(), guest.clone()], &c).len(), 2);
        c.clear();
        assert_eq!(canonical_roster(vec![configured, guest], &c).len(), 2, "never merge by localhost or similar names");
    }

    #[test]
    fn announced_guest_joins_the_roster_without_a_file_entry() {
        std::env::set_var("KASATERM_MACHINES", r#"[{"label":"명부","ssh":"listed"}]"#);
        announce_guest("손님", 19123, "guest.local", "/Users/guest", "abc123");
        let all = machines();
        let g = all.iter().find(|m| m.label == "손님").expect("알려 온 기계가 목록에 선다");
        assert!(g.guest);
        assert_eq!(g.base, "http://127.0.0.1:19123");
        assert!(!g.tunneled, "알려 온 기계로는 이쪽이 터널을 들지 않는다");
        assert!(g.roots.iter().any(|(_, r)| r == "/Users/guest"));
        // 파일 항목과 이름이 겹치면 파일 쪽이 이긴다.
        announce_guest("명부", 19124, "", "", "");
        let listed = machines().into_iter().filter(|m| m.label == "명부").collect::<Vec<_>>();
        assert_eq!(listed.len(), 1);
        assert!(!listed[0].guest);
        std::env::remove_var("KASATERM_MACHINES");
    }

    #[test]
    fn reverse_port_is_stable_and_out_of_the_forward_range() {
        assert_eq!(reverse_port("맥북"), reverse_port("맥북"));
        let p = reverse_port("맥북");
        assert!((19000..19500).contains(&p));
        assert!(!(18900..18990).contains(&tunnel_port("맥북")) || p != tunnel_port("맥북"));
    }

    #[test]
    fn direct와_uplink_생존을_구분한다() {
        assert_eq!(availability(Some(Duration::from_secs(1)), false), (true, Some("direct")));
        assert_eq!(availability(Some(STALE_AFTER), true), (true, Some("uplink")));
        assert_eq!(availability(None, true), (true, Some("uplink")));
        assert_eq!(availability(None, false), (false, None));
    }

    #[test]
    fn uplink_only는_online이지만_stale_direct_자료를_되살리지_않는다() {
        let stale = Seen {
            at: Instant::now() - STALE_AFTER,
            panes: vec![serde_json::json!({"id":"old"})],
            sync: true,
            build: Some("same-build".to_string()),
            machine_id: Some("stable-mini-1".to_string()),
        };
        let uplink = crate::uplink::GatewayMachine {
            id: "stable-mini-1".to_string(),
            machine: "nachoneko".to_string(),
            aliases: vec!["nachoneko".to_string()],
        };
        let relayed = snapshot_machine(m(), Some(&stale), Some(&uplink), "same-build");
        assert_eq!(relayed["online"], true);
        assert_eq!(relayed["online_via"], "uplink");
        assert_eq!(relayed["route"], "~stable-mini-1");
        assert_eq!(relayed["panes"], serde_json::json!([]));
        assert!(relayed["build"].is_null());
        assert_eq!(relayed["build_match"], false);

        let down = snapshot_machine(m(), Some(&stale), None, "same-build");
        assert_eq!(down["online"], false);
        assert!(down["online_via"].is_null());
    }

    #[test]
    fn 표시별명과_달라도_학습한_stable_id로만_잇는다() {
        let seen = Seen {
            at: Instant::now() - STALE_AFTER,
            panes: Vec::new(),
            sync: true,
            build: None,
            machine_id: Some("stable-mini-1".to_string()),
        };
        let live = vec![crate::uplink::GatewayMachine {
            id: "stable-mini-1".to_string(),
            machine: "nachoneko".to_string(),
            aliases: vec!["nachoneko.local".to_string()],
        }];
        let machine = m();
        assert_eq!(machine.label, "미니");
        assert_eq!(matching_uplink(&machine, Some(&seen), &live).map(|up| up.id.as_str()), Some("stable-mini-1"));

        let wrong = vec![crate::uplink::GatewayMachine {
            id: "other-machine".to_string(),
            machine: "nachoneko".to_string(),
            aliases: Vec::new(),
        }];
        assert!(matching_uplink(&machine, Some(&seen), &wrong).is_none());
    }

    #[test]
    fn 같은_별명이_서로_다른_id면_uplink_online으로_치지_않는다() {
        let machine = m();
        let live = vec![
            crate::uplink::GatewayMachine {
                id: "mini-one".to_string(),
                machine: "미니".to_string(),
                aliases: Vec::new(),
            },
            crate::uplink::GatewayMachine {
                id: "mini-two".to_string(),
                machine: "미니".to_string(),
                aliases: Vec::new(),
            },
        ];
        assert!(matching_uplink(&machine, None, &live).is_none());
    }

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
