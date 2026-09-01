//! 기계 명부 — 학생을 옮겨 다니게 할 「다른 기계」들의 정본.
//!
//! remoteboard(원격 kasaterm 의 board 를 합쳐 오는 것)와 별개다. 여기 항목은
//! **pane 호스트(kasa-serve-web)의 주소**이고, 이사(migrate)의 목적지·아로나
//! 이사 탭의 기계 목록·경로 매핑(roots)이 전부 이 파일 하나를 본다.
//!
//! 설정: `~/.config/kasaterm/machines.json`
//! ```json
//! [{"label":"맥미니","base":"http://127.0.0.1:18791",
//!   "roots":{"/Users/kasa/Desktop/momewomo":"/Users/miku/momewomo"}}]
//! ```
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
}

fn parse(v: &Value) -> Vec<Machine> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|m| {
            let label = m.get("label")?.as_str()?.trim().to_string();
            let base = m
                .get("base")?
                .as_str()?
                .trim()
                .trim_end_matches('/')
                .to_string();
            if label.is_empty() || base.is_empty() {
                return None;
            }
            let host = m
                .get("host")
                .and_then(|h| h.as_str())
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty())
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
            })
        })
        .collect()
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
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    let path = std::path::Path::new(&home).join(".config/kasaterm/machines.json");
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
