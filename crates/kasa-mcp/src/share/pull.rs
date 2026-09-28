//! 다른 기기에서 끌어오기. 보내는 쪽은 목록과 파일만 내주고(`serve`), 판정·쓰기는
//! 늘 받는 쪽이 한다 — 쓰는 곳이 한 곳이라 두 기기가 서로 밀어 넣다 엉키지 않는다.

use super::path;
use super::version::{decide, Decision, Entry};
use super::{Engine, PeerStatus};
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// 관문은 큐가 넘치면 본문을 자른 채 정상 종료로 보낸다. 그래서 조각으로 받고, 모자라면
/// 받은 데까지 이어서 다시 묻는다.
const CHUNK: u64 = 4 << 20;

pub(super) struct Peer {
    pub id: String,
    pub label: String,
    pub base: String,
}

/// 살아 있는 다른 기기 — 보드와 같은 길(직통 먼저, 없으면 관문 우회)로 닿는다.
pub(super) fn peers(me: &str) -> Vec<Peer> {
    let mut out: Vec<Peer> = Vec::new();
    for m in crate::machines::snapshot() {
        if m["online"] != true {
            continue;
        }
        let Some(id) = m["route"].as_str().and_then(|r| r.strip_prefix('~')) else { continue };
        if id == me || out.iter().any(|p| p.id == id) {
            continue;
        }
        let direct = (m["online_via"] == "direct")
            .then(|| m["base"].as_str().filter(|b| !b.is_empty()).map(str::to_string))
            .flatten();
        let base = crate::board_service::known_route(id)
            .or(direct)
            .or_else(|| crate::board_service::relay_base(id));
        if let Some(base) = base {
            let label = m["label"].as_str().unwrap_or(id).to_string();
            out.push(Peer { id: id.to_string(), label, base: base.trim_end_matches('/').to_string() });
        }
    }
    out
}

fn request(client: &reqwest::Client, peer: &Peer, path: &str) -> Result<reqwest::RequestBuilder, String> {
    let url = reqwest::Url::parse(&format!("{}{path}", peer.base)).map_err(|e| e.to_string())?;
    let req = client.get(url);
    Ok(match crate::remote::connection_auth_token(&peer.base) {
        Some(token) => req.header("x-kasa-token", token),
        None => req,
    })
}

pub(super) async fn pull(engine: &Engine, client: &reqwest::Client, peer: &Peer) -> PeerStatus {
    let prev = engine.status().peers.into_iter().find(|p| p.id == peer.id);
    let mut st = PeerStatus {
        id: peer.id.clone(),
        label: peer.label.clone(),
        last_ok_ms: prev.map_or(0, |p| p.last_ok_ms),
        ..Default::default()
    };
    match pull_inner(engine, client, peer).await {
        Ok(n) => {
            st.ok = true;
            st.pulled = n;
            st.last_ok_ms = super::now_ms();
        }
        Err(e) => st.error = Some(e),
    }
    st
}

async fn pull_inner(engine: &Engine, client: &reqwest::Client, peer: &Peer) -> Result<usize, String> {
    let cursor = engine.with_index(|i| i.peers.get(&peer.id).cloned().unwrap_or_default());
    let url = format!("/term/share/manifest?since={}&epoch={}", cursor.seq, cursor.epoch);
    let resp = request(client, peer, &url)?
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("그 기기 카사텀이 옛 판이라 KASA-share 가 없어요".into());
    }
    if !resp.status().is_success() {
        return Err(format!("목록을 못 받았어요 ({})", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| e.to_string())?;
    if body["machine_id"].as_str() != Some(peer.id.as_str()) {
        return Err("다른 기기가 대답했어요".into());
    }
    let epoch = body["epoch"].as_str().unwrap_or_default().to_string();
    let seq = body["seq"].as_u64().unwrap_or(0);
    let entries: Vec<Entry> = serde_json::from_value(body["entries"].clone()).map_err(|e| e.to_string())?;

    let mut skipped = Vec::new();
    let plan: Vec<(String, Decision)> = engine.with_index(|idx| {
        entries
            .iter()
            .filter(|e| {
                let ok = path::safe(&e.path) && (!cfg!(windows) || path::portable(&e.path)) && e.size <= super::MAX_FILE;
                if !ok {
                    skipped.push(e.path.clone());
                }
                ok
            })
            .map(|e| {
                let key = path::key(&e.path);
                let d = decide(idx.entries.get(&key), e);
                (key, d)
            })
            .filter(|(_, d)| *d != Decision::Skip)
            .collect()
    });
    if !skipped.is_empty() {
        if let Ok(mut st) = engine.status.lock() {
            for p in skipped {
                if !st.skipped.contains(&p) {
                    st.skipped.push(p);
                }
            }
        }
    }

    let (mut done, mut all) = (0, true);
    let mut last_err = None;
    for (key, d) in plan {
        match execute(engine, client, peer, &key, d).await {
            Ok(true) => done += 1,
            Ok(false) => all = false,
            Err(e) => {
                all = false;
                last_err = Some(e);
            }
        }
    }
    // 하나라도 못 했으면 커서를 안 옮긴다 — 다음 판에 같은 목록을 다시 보고, 판정은 몇 번을 해도 같다.
    if all {
        engine.with_index(|i| i.peers.insert(peer.id.clone(), super::Cursor { epoch, seq }));
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(done),
    }
}

async fn execute(engine: &Engine, client: &reqwest::Client, peer: &Peer, key: &str, d: Decision) -> Result<bool, String> {
    let io = |e: std::io::Error| e.to_string();
    match d {
        Decision::Skip => Ok(true),
        Decision::Adopt(e) => {
            engine.with_index(|i| i.record(e));
            Ok(true)
        }
        Decision::Fetch(e) => {
            let part = fetch(engine, client, peer, &e).await?;
            engine.place(key, &e, &part).map_err(io)
        }
        Decision::Trash(e) => engine.trash(key, &e).map_err(io),
        Decision::KeepBoth { entry, loser, local_wins } => {
            let copy = path::conflict_name(&loser.path, &loser.origin, |k| engine.taken(k));
            if local_wins {
                let part = fetch(engine, client, peer, &loser).await?;
                engine.place_copy(&copy, &part).map_err(io)?;
                engine.with_index(|i| i.record(entry));
                Ok(true)
            } else {
                let part = fetch(engine, client, peer, &entry).await?;
                if !engine.step_aside(key, &copy).map_err(io)? {
                    return Ok(false);
                }
                engine.place(key, &entry, &part).map_err(io)
            }
        }
    }
}

/// `.kasaterm/tmp/<sha>.part` 로 받고 sha 가 맞을 때만 돌려준다. 끊겨도 받은 데부터 잇는다.
async fn fetch(engine: &Engine, client: &reqwest::Client, peer: &Peer, e: &Entry) -> Result<PathBuf, String> {
    if e.sha.len() != 64 || !e.sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{} 의 sha 가 이상해요", e.path));
    }
    let part = engine.meta("tmp").join(format!("{}.part", e.sha));
    let mut have = std::fs::metadata(&part).map_or(0, |m| m.len());
    if have > e.size {
        have = 0;
        let _ = std::fs::remove_file(&part);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .map_err(|x| x.to_string())?;
    while have < e.size {
        let len = CHUNK.min(e.size - have);
        let mut url = reqwest::Url::parse(&format!("{}/term/share/file", peer.base)).map_err(|x| x.to_string())?;
        url.query_pairs_mut()
            .append_pair("path", &e.path)
            .append_pair("sha", &e.sha)
            .append_pair("offset", &have.to_string())
            .append_pair("len", &len.to_string());
        let req = client.get(url).timeout(Duration::from_secs(120));
        let req = match crate::remote::connection_auth_token(&peer.base) {
            Some(token) => req.header("x-kasa-token", token),
            None => req,
        };
        let resp = req.send().await.map_err(|x| x.to_string())?;
        if resp.status() == reqwest::StatusCode::CONFLICT {
            return Err(format!("{} 이 {} 에서 그새 바뀌었어요", e.path, peer.label));
        }
        if !resp.status().is_success() {
            return Err(format!("{} 을 못 받았어요 ({})", e.path, resp.status()));
        }
        let bytes = resp.bytes().await.map_err(|x| x.to_string())?;
        if bytes.is_empty() {
            return Err(format!("{} 조각이 비어 왔어요", e.path));
        }
        let take = bytes.len().min(len as usize);
        file.write_all(&bytes[..take]).map_err(|x| x.to_string())?;
        have += take as u64;
    }
    drop(file);
    let got = {
        let part = part.clone();
        tokio::task::spawn_blocking(move || super::sha256_file(&part)).await.map_err(|x| x.to_string())?
    };
    if got.ok().as_deref() != Some(e.sha.as_str()) {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{} 의 내용이 목록과 달라 버렸어요", e.path));
    }
    Ok(part)
}
