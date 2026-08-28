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
    /// 로컬 경로 → 그 기계 경로. 긴 접두부터 맞춘다 — nacho-neko 처럼 부모와
    /// 다른 자리에 사는 레포를 부모 규칙보다 먼저 잡기 위해서다.
    pub roots: Vec<(String, String)>,
}

fn parse(v: &Value) -> Vec<Machine> {
    let Some(arr) = v.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|m| {
            let label = m.get("label")?.as_str()?.trim().to_string();
            let base = m.get("base")?.as_str()?.trim().trim_end_matches('/').to_string();
            if label.is_empty() || base.is_empty() {
                return None;
            }
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
            Some(Machine { label, base, roots })
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
    let Ok(home) = std::env::var("HOME") else { return Vec::new() };
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

/// 주소로 라벨 역조회 — surface.remote 처럼 주소만 들고 들어온 링크에 이름을
/// 붙여 준다. 명부 밖 주소면 None.
pub fn label_for_base(base: &str) -> Option<String> {
    let b = base.trim_end_matches('/');
    machines().into_iter().find(|m| m.base == b).map(|m| m.label)
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

/// 캐시: 라벨 → (마지막으로 닿은 시각, /term/panes 행들).
type Cache = HashMap<String, (Instant, Vec<Value>)>;

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
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
    serde_json::from_str::<Value>(&text).ok()?.as_array().cloned()
}

/// 백그라운드 폴링. 명부가 비어 있으면 루프를 아예 안 돈다(remoteboard 규율).
pub async fn poll_loop() {
    let list = machines();
    if list.is_empty() {
        return;
    }
    eprintln!(
        "[machines] {} 곳 폴링: {}",
        list.len(),
        list.iter().map(|m| m.label.as_str()).collect::<Vec<_>>().join(", ")
    );
    let client = reqwest::Client::new();
    loop {
        for m in &list {
            if let Some(panes) = fetch_panes(&client, &m.base).await {
                if let Ok(mut c) = cache().lock() {
                    c.insert(m.label.clone(), (Instant::now(), panes));
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
            let age = hit.map(|(at, _)| at.elapsed());
            let online = age.is_some_and(|a| a < STALE_AFTER);
            serde_json::json!({
                "label": m.label,
                "base": m.base,
                "online": online,
                "ago_secs": age.map(|a| a.as_secs()),
                "panes": if online {
                    hit.map(|(_, p)| p.clone()).unwrap_or_default()
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
