//! 다른 기계의 board 를 물어다 이 기계 것과 **한 목록으로** 내주는 자리.
//!
//! 왜 백엔드에서 합치나 — 프론트(`web/arona-ui/src/lib/mcp.ts`)는 `BASE` **하나**를
//! 골라 쓰고 호출부가 50곳 가까이 된다. 「두 백엔드를 동시에」로 가려면 그 50곳을
//! 학생별 origin 으로 바꿔야 하고, 학생마다 어느 기계인지를 화면 상태에 계속 들고
//! 다녀야 한다. 여기서 합쳐 주면 프론트는 한 글자도 안 고쳐도 된다.
//!
//! ⚠️ **요청이 올 때 원격을 물으면 안 된다.** 아로나는 board 를 1~2초마다 폴링하는데,
//! 원격이 죽어 있으면 그 폴링마다 타임아웃만큼 로컬 board 까지 멈춘다 — 남의 기계가
//! 꺼졌다고 내 화면이 굳는 건 어떤 타임아웃 값으로도 정당화가 안 된다. 그래서 백그라운드
//! 루프가 미리 받아 두고, 요청은 **캐시만 즉시** 읽는다(원격이 죽으면 0ms 로 로컬만 나온다).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

/// 폴링 주기. 아로나 폴링(1~2초)보다 느슨해도 되는 이유는 캐시가 그 사이를 메우기
/// 때문이다 — 화면은 늘 즉시 그려지고, 늦는 건 원격 학생의 상태 신선도뿐이다.
const POLL_EVERY: Duration = Duration::from_secs(3);
/// 한 번 물어볼 때 기다리는 한계. 캐시 루프 안이라 이게 길어도 화면은 안 굳는다.
const FETCH_TIMEOUT: Duration = Duration::from_secs(4);
/// 이보다 오래된 캐시는 안 내준다. 원격이 죽으면 그 학생들이 목록에서 **사라져야**
/// 한다 — 죽은 기계의 학생이 「idle」로 남아 있으면 거기 일을 시키게 된다.
const STALE_AFTER: Duration = Duration::from_secs(20);

/// 합쳐 올 상대 한 곳.
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    /// 화면과 id 접두에 쓰는 짧은 이름(예: `맥미니`).
    pub label: String,
    /// `http://host:port` — 뒤에 `/board` 를 붙여 부른다.
    pub base: String,
}

/// 캐시: 라벨 → (받은 시각, board 행, background 세션).
type Cache = HashMap<String, (Instant, Vec<Value>, Vec<Value>)>;

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 설정된 원격 목록. env 가 먼저고, 없으면 설정 파일을 본다. 둘 다 없으면 빈 목록이고
/// 그때 이 모듈은 아무 일도 하지 않는다(지금과 완전히 같은 동작).
///
/// - env `KASATERM_REMOTE_BOARDS="맥미니=http://localhost:8766,저기=http://x:8766"`
/// - 파일 `~/.config/kasaterm/remote-boards.json` = `[{"label":"맥미니","base":"http://localhost:8766"}]`
///
/// env 를 먼저 보는 이유는 검증용 인스턴스가 사용자 설정을 안 건드리고 자기 원격을
/// 가리킬 수 있어야 해서다(다른 격리 env 들과 같은 규율).
pub fn remotes() -> Vec<Remote> {
    if let Ok(s) = std::env::var("KASATERM_REMOTE_BOARDS") {
        let v = parse_env(&s);
        if !v.is_empty() {
            return v;
        }
    }
    let Ok(home) = std::env::var("HOME") else { return Vec::new() };
    let path = std::path::Path::new(&home).join(".config/kasaterm/remote-boards.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    parse_json(&text)
}

/// `라벨=주소` 를 쉼표로 이은 형식. 라벨이나 주소가 비면 그 항목만 버린다 — 오타 하나로
/// 목록 전체를 잃는 것보다 낫다.
fn parse_env(s: &str) -> Vec<Remote> {
    s.split(',')
        .filter_map(|part| {
            let (label, base) = part.split_once('=')?;
            let (label, base) = (label.trim(), base.trim().trim_end_matches('/'));
            (!label.is_empty() && !base.is_empty()).then(|| Remote {
                label: label.to_string(),
                base: base.to_string(),
            })
        })
        .collect()
}

fn parse_json(text: &str) -> Vec<Remote> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(arr) = v.as_array() else { return Vec::new() };
    arr.iter()
        .filter_map(|e| {
            let label = e.get("label")?.as_str()?.trim();
            let base = e.get("base")?.as_str()?.trim().trim_end_matches('/');
            (!label.is_empty() && !base.is_empty()).then(|| Remote {
                label: label.to_string(),
                base: base.to_string(),
            })
        })
        .collect()
}

/// done 이 아닌 원격 항목은 전부 싣고, done 은 이만큼만 싣는다.
///
/// 원격 board 는 `claude agents --json --all` 을 옮긴 것이라 **끝난 세션이 계속 쌓인다**
/// (2026-08-26 실측: 14건이 전부 done). 살아 있는 pane 만 담는 로컬 board 와 성격이
/// 달라서, 그대로 합치면 시체가 보드를 덮고 정작 일을 시킬 학생이 안 보인다.
/// 그렇다고 done 을 통째로 버리면 「방금 뭘 끝냈나」를 못 보므로 몇 개만 남긴다.
const DONE_KEEP: usize = 5;

/// 원격 pane 하나를 이 기계 목록에 섞을 수 있게 표시한다.
///
/// ⚠️ **`character` 를 기계 이름으로 갈아 끼운다.** 원격은 이 칸이 비어 있는 게 아니라
/// **세션 요약 영어가 들어차 있다**(실측: `"arithmetic calculation"`) — 그대로 두면
/// 프론트가 그 문자열로 아바타를 찾으러 갔다가 조용히 이니셜로 떨어진다.
///
/// 지우고 `title` 폴백에 맡기면 이름표가 「1+1 계산」이 되는데, 그러면 **어느 기계
/// 학생인지가 화면 어디에도 안 남는다** — 합치는 목적이 「경계를 없애는 것」이지
/// 「출처를 지우는 것」이 아니다. 말을 걸려면 결국 어느 기계인지 알아야 한다.
/// 기계 이름을 넣으면 이름표가 「맥미니 · 1+1 계산」이 되고 아바타는 「맥」으로
/// 떨어진다(로스터에 없는 이름이라 강조색은 순환색 그대로). 프론트 수정 0줄.
///
/// ⚠️ **`surface_id` 는 `%` 로 시작할 때만 접두를 붙인다.** 지금 원격(standalone)은
/// pane 이 없어 세션 UUID 를 쓰므로 로컬 `%1`·`%2` 와 겹칠 수가 없고, 그래서 「`%` 로
/// 시작하면 로컬, 아니면 원격」이라는 규칙만으로 상세를 어디에 물을지 가를 수 있다 —
/// 매핑표가 필요 없다. 그 규칙을 지키려고 UUID 는 건드리지 않는다.
/// 반대로 원격이 pane 을 가진 kasaterm 본체라면 `%1` 이 양쪽에 생겨 **로컬 pane 자리에
/// 남의 기계 학생이 들어앉는다**(아로나 그리드가 `layoutRects` 를 돌며
/// `agents.find(id === surface_id)` 로 짝을 짓는다). 그 경우만 접두로 갈라 준다.
fn tag(mut row: Value, label: &str) -> Value {
    let Some(obj) = row.as_object_mut() else { return row };
    if let Some(id) = obj.get("surface_id").and_then(|v| v.as_str()) {
        if id.starts_with('%') {
            let tagged = format!("{label}:{id}");
            obj.insert("surface_id".into(), Value::String(tagged));
        }
    }
    obj.insert("machine".into(), Value::String(label.to_string()));
    obj.insert("character".into(), Value::String(label.to_string()));
    // 말 거는 길이 아직 없다. `"message"` 로 두면 board 를 읽는 쪽이 SendMessage 로
    // 닿는다고 믿는데, 그 이름은 이 기계 명부에 없어서 **오류 없이 사라진다**.
    obj.insert("reach".into(), Value::String("remote".into()));
    obj.remove("peer_name");
    row
}

/// done 시체를 잘라낸다. 순서는 원격이 준 그대로를 믿는다 — `claude agents` 의 정렬을
/// 여기서 다시 판단할 근거가 없고, 어차피 상한을 두는 게 목적이라 어느 5개인지는
/// 두 번째 문제다.
fn trim_done(rows: Vec<Value>) -> Vec<Value> {
    let mut done_seen = 0usize;
    rows.into_iter()
        .filter(|r| {
            if r.get("status").and_then(|v| v.as_str()) != Some("done") {
                return true;
            }
            done_seen += 1;
            done_seen <= DONE_KEEP
        })
        .collect()
}

/// background 세션에도 같은 표시를 얹는다. 이쪽 식별자는 `sessionId`/`pid` 라
/// surface_id 충돌 문제는 없지만, 어느 기계 것인지는 똑같이 보여야 한다.
fn tag_agent(mut a: Value, label: &str) -> Value {
    let Some(obj) = a.as_object_mut() else { return a };
    obj.insert("machine".into(), Value::String(label.to_string()));
    // 부모 pane 은 그 기계의 pane 이다 — 접두를 안 붙이면 프론트가 이 기계의 같은
    // 이름 pane 을 부모로 짚는다.
    if let Some(p) = obj.get("parentSurface").and_then(|v| v.as_str()) {
        let tagged = format!("{label}:{p}");
        obj.insert("parentSurface".into(), Value::String(tagged));
    }
    a
}

/// 한 주소를 GET 해서 JSON 으로. `reqwest` 가 `json` feature 없이 들어와 있어
/// (`default-features = false`) `.json()` 이 없다 — 그것 하나 때문에 의존성을 늘리는
/// 대신 본문을 받아 직접 판다.
async fn get_json(client: &reqwest::Client, url: String) -> Option<Value> {
    let text = client.get(url).timeout(FETCH_TIMEOUT).send().await.ok()?.text().await.ok()?;
    serde_json::from_str(&text).ok()
}

async fn fetch_one(client: &reqwest::Client, r: &Remote) -> Option<(Vec<Value>, Vec<Value>)> {
    let board: Vec<Value> = get_json(client, format!("{}/board", r.base))
        .await?
        .get("board")?
        .as_array()?
        .iter()
        .map(|row| tag(row.clone(), &r.label))
        .collect();
    let board = trim_done(board);
    // background 는 없어도 board 는 살린다 — 둘을 한 실패로 묶으면 한쪽 실패가
    // 멀쩡한 다른 쪽까지 지운다.
    let agents = get_json(client, format!("{}/background-agents", r.base))
        .await
        .and_then(|v| v.get("agents")?.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .map(|a| tag_agent(a, &r.label))
        .collect();
    Some((board, agents))
}

/// 백그라운드 폴링. 원격이 하나도 설정 안 됐으면 **루프를 아예 안 돈다** — 설정 없는
/// 사람에게 3초마다 도는 태스크를 남기지 않는다.
pub async fn poll_loop() {
    let list = remotes();
    if list.is_empty() {
        return;
    }
    eprintln!(
        "[remote-board] {} 곳 폴링: {}",
        list.len(),
        list.iter().map(|r| r.label.as_str()).collect::<Vec<_>>().join(", ")
    );
    let client = reqwest::Client::new();
    loop {
        for r in &list {
            if let Some((board, agents)) = fetch_one(&client, r).await {
                if let Ok(mut c) = cache().lock() {
                    c.insert(r.label.clone(), (Instant::now(), board, agents));
                }
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// 캐시에 든 원격 board 행. 오래된 것은 안 준다.
pub fn board_rows() -> Vec<Value> {
    let Ok(c) = cache().lock() else { return Vec::new() };
    c.values()
        .filter(|(at, _, _)| at.elapsed() < STALE_AFTER)
        .flat_map(|(_, rows, _)| rows.iter().cloned())
        .collect()
}

/// 캐시에 든 원격 background 세션. 오래된 것은 안 준다.
pub fn background_agents() -> Vec<Value> {
    let Ok(c) = cache().lock() else { return Vec::new() };
    c.values()
        .filter(|(at, _, _)| at.elapsed() < STALE_AFTER)
        .flat_map(|(_, _, agents)| agents.iter().cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_형식을_읽고_빈_항목은_버린다() {
        let v = parse_env("맥미니=http://localhost:8766/, ,저기=http://x:1/, =http://y:2, 라벨만=");
        assert_eq!(
            v,
            vec![
                Remote { label: "맥미니".into(), base: "http://localhost:8766".into() },
                Remote { label: "저기".into(), base: "http://x:1".into() },
            ]
        );
    }

    #[test]
    fn json_형식도_같은_결과를_준다() {
        let v = parse_json(r#"[{"label":"맥미니","base":"http://localhost:8766"}]"#);
        assert_eq!(v, vec![Remote { label: "맥미니".into(), base: "http://localhost:8766".into() }]);
        // 깨진 파일은 빈 목록 — 설정이 잘못됐다고 board 가 죽으면 안 된다.
        assert!(parse_json("{{{").is_empty());
        assert!(parse_json(r#"[{"label":"x"}]"#).is_empty());
    }

    #[test]
    fn pane_id_원격만_접두가_붙는다() {
        let t = tag(serde_json::json!({ "surface_id": "%1" }), "맥미니");
        assert_eq!(t["surface_id"], "맥미니:%1");
    }

    #[test]
    fn uuid_원격은_id_를_안_건드린다() {
        // 「`%` 로 시작하면 로컬」 규칙으로 상세를 가를 수 있게 남겨 둔다.
        let id = "d960e377-f065-4b80-8066-1ba4f5a05d42";
        let t = tag(serde_json::json!({ "surface_id": id }), "맥미니");
        assert_eq!(t["surface_id"], id);
    }

    #[test]
    fn 원격_행은_말걸기가_막히고_캐릭터_자리에_기계가_들어간다() {
        let row = serde_json::json!({
            "surface_id": "abc-uuid",
            "status": "idle",
            "reach": "message",
            "peer_name": "hina-p1-abc",
            "character": "arithmetic calculation",
            "title": "1+1 계산",
        });
        let t = tag(row, "맥미니");
        assert_eq!(t["machine"], "맥미니");
        assert_eq!(t["reach"], "remote");
        // peer_name 이 남으면 board 를 읽는 쪽이 SendMessage 로 닿는다고 믿는다.
        assert!(t.get("peer_name").is_none());
        // 세션 요약 영어가 남으면 프론트가 그 문자열로 아바타를 찾으러 간다.
        // 지우는 대신 기계 이름을 넣어 이름표가 「맥미니 · 1+1 계산」이 되게 한다.
        assert_eq!(t["character"], "맥미니");
        // 나머지 칸은 그대로 — 합치기는 표시만 얹고 내용을 고치지 않는다.
        assert_eq!(t["status"], "idle");
        assert_eq!(t["title"], "1+1 계산");
    }

    #[test]
    fn done_은_상한까지만_살아남고_나머지는_다_실린다() {
        let mut rows: Vec<Value> = (0..9)
            .map(|i| serde_json::json!({ "surface_id": format!("d{i}"), "status": "done" }))
            .collect();
        rows.insert(4, serde_json::json!({ "surface_id": "live", "status": "working" }));
        let out = trim_done(rows);
        // 살아 있는 것은 무조건 남는다 — 상한은 done 에만 건다.
        assert!(out.iter().any(|r| r["surface_id"] == "live"));
        let done = out.iter().filter(|r| r["status"] == "done").count();
        assert_eq!(done, DONE_KEEP);
        assert_eq!(out.len(), DONE_KEEP + 1);
    }

    #[test]
    fn background_은_부모_pane_에도_접두가_붙는다() {
        let a = serde_json::json!({ "sessionId": "abc", "parentSurface": "%3" });
        let t = tag_agent(a, "맥미니");
        assert_eq!(t["parentSurface"], "맥미니:%3");
        assert_eq!(t["machine"], "맥미니");
    }

    #[test]
    fn surface_id_가_없는_행도_안_죽는다() {
        let t = tag(serde_json::json!({ "status": "idle" }), "맥미니");
        assert_eq!(t["machine"], "맥미니");
        assert!(t.get("surface_id").is_none());
    }
}
