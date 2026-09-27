//! 관문 — 중계소(`kasa-relay`)에 얹혀 **폰 주소를 기계로 잇는** 쪽. 앱 쪽과 프레임
//! 규약은 `uplink.rs`(그 파일 머리말이 왜 이 구조인지 말한다).
//!
//! - `GET /relay/uplink` (WS): 앱이 붙어 `hello{key, slugs, machine}` 를 보낸다. slug 는
//!   처음 본 키에 묶이고(디스크에 남긴다 — 재시작을 넘게), 다른 키가 같은 slug 를 대면
//!   그 slug 만 거절한다. 같은 slug 가 다시 붙으면 새 연결이 이긴다(앱 재시작).
//! - `/u/<slug>/…` (HTTP·WS 전부): slug 의 업링크를 찾아 요청을 스트림 하나로 내려보내고
//!   답을 그대로 폰에 준다. 업링크가 없으면 「그 기계가 지금 안 붙어 있다」 화면.
//!
//! 관문은 **자격을 모른다.** 페이지·pane·유저 관리는 전부 그 앱이 자기 로컬 서버에서
//! 한다(`mobile.rs`). 여기 있는 건 배관뿐이다 — 릴레이 토큰도 안 본다(공용 문).
//! 대신 배관이 자격을 **새게 하지는 않는다**: 경로가 `/u/<slug>/` 밖으로 못 나가게 하고,
//! 폰이 실어 온 토큰·앱이 심는 쿠키는 어느 쪽으로도 넘기지 않는다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Path as AxPath, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{any, get},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::uplink::{
    decode, encode, safe_path, skip_header, BODY, CLOSE, END, HEAD, OPEN, STREAM_QUEUE, WS_BIN, WS_PING, WS_PONG, WS_TEXT,
};

type Frame = (u8, Vec<u8>);

/// 관문은 인증 없이 열려 있다 — 누구나 업링크를 세울 수 있으니 자원에 상한을 둔다.
const MAX_SLUGS_PER_HELLO: usize = 32;
const MAX_UPLINKS_PER_KEY: usize = 8;
const MAX_UPLINKS: usize = 256;
const MAX_STREAMS_PER_UPLINK: usize = 512;
const MAX_BODY: usize = 64 * 1024 * 1024;
/// 이만큼 아무도 안 쓴 주소 묶음은 상태 파일에서 걷는다.
const SLUG_RETENTION_SECS: u64 = 90 * 24 * 3600;

struct Uplink {
    conn: u64,
    machine: String,
    machine_id: String,
    aliases: Vec<String>,
    last_seen: Mutex<Instant>,
    tx: mpsc::Sender<Message>,
    streams: Mutex<HashMap<u32, mpsc::Sender<Frame>>>,
    next: AtomicU32,
}

const UPLINK_STALE_AFTER: Duration = Duration::from_secs(75);

impl Uplink {
    fn fresh(&self) -> bool {
        self.last_seen
            .lock()
            .is_ok_and(|seen| seen.elapsed() < UPLINK_STALE_AFTER)
    }

    fn touch(&self) {
        if let Ok(mut seen) = self.last_seen.lock() {
            *seen = Instant::now();
        }
    }
}

#[derive(Clone, Copy)]
struct Candidate<'a> {
    index: usize,
    machine: &'a str,
    machine_id: &'a str,
    aliases: &'a [String],
    fresh: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RoutePick {
    /// 대상 기계의 업링크를 직접 쓴다 — `/m/<기계>` 접두를 벗긴다.
    Machine(usize),
    /// 대상 업링크가 없어 기존 최신 업링크의 로컬 direct proxy로 보낸다.
    Fallback(usize),
    Missing,
    Ambiguous,
}

fn pick_route(candidates: &[Candidate<'_>], target: Option<&str>) -> RoutePick {
    let default = candidates
        .iter()
        .rev()
        .find(|candidate| candidate.fresh)
        .map(|candidate| candidate.index);
    let Some(target) = target else {
        return default.map_or(RoutePick::Missing, RoutePick::Fallback);
    };
    let stable_id = target.strip_prefix('~');
    let matching: Vec<&Candidate<'_>> = candidates
        .iter()
        .filter(|candidate| {
            candidate.fresh
                && stable_id.map_or_else(
                    || {
                        candidate.machine == target
                            || candidate.aliases.iter().any(|alias| alias == target)
                    },
                    |id| candidate.machine_id == id,
                )
        })
        .collect();
    let Some(last) = matching.last() else {
        if stable_id.is_some() {
            return RoutePick::Missing;
        }
        return default.map_or(RoutePick::Missing, RoutePick::Fallback);
    };
    if matching
        .iter()
        .any(|candidate| candidate.machine_id != last.machine_id)
    {
        return RoutePick::Ambiguous;
    }
    RoutePick::Machine(last.index)
}

fn live_machines(candidates: &[Candidate<'_>]) -> Vec<crate::uplink::GatewayMachine> {
    let mut by_id: HashMap<&str, crate::uplink::GatewayMachine> = HashMap::new();
    for candidate in candidates.iter().filter(|candidate| {
        candidate.fresh && !candidate.machine.is_empty() && !candidate.machine_id.starts_with("legacy-")
    }) {
        by_id.insert(
            candidate.machine_id,
            crate::uplink::GatewayMachine {
                id: candidate.machine_id.to_string(),
                machine: candidate.machine.to_string(),
                aliases: candidate.aliases.to_vec(),
            },
        );
    }
    let mut machines: Vec<_> = by_id.into_values().collect();
    machines.sort_by(|a, b| a.id.cmp(&b.id));
    machines
}

fn machine_route(rest: &str) -> Option<(&str, &str)> {
    let route = rest.strip_prefix("m/")?;
    let (machine, tail) = route.split_once('/')?;
    (!machine.is_empty() && !tail.is_empty()).then_some((machine, tail))
}

fn candidates_of(uplinks: &[Arc<Uplink>]) -> Vec<Candidate<'_>> {
    uplinks
        .iter()
        .enumerate()
        .map(|(index, uplink)| Candidate {
            index,
            machine: &uplink.machine,
            machine_id: &uplink.machine_id,
            aliases: &uplink.aliases,
            fresh: uplink.fresh(),
        })
        .collect()
}

#[derive(Clone)]
pub struct Gate {
    /// slug → 그 slug 를 알린 **살아 있는 연결들**(뒤가 최신). 하나만 두면 같은
    /// 기계에서 앱이 둘 뜬 경우(검증 리그·재시작 겹침)에 나중 것이 떠나며 먼저 것을
    /// 함께 떼어 버린다 — 2026-09-02 실측: 리그가 붙었다 떠나자 실앱 주소가 502.
    by_slug: Arc<Mutex<HashMap<String, Vec<Arc<Uplink>>>>>,
    /// slug → 키 해시. 처음 온 키가 주인이다.
    keys: Arc<Mutex<HashMap<String, SlugRec>>>,
    /// 살아 있는 업링크 연결 → 키 해시. 상한 판정용.
    conns: Arc<Mutex<HashMap<u64, String>>>,
    state_path: Option<PathBuf>,
    seq: Arc<AtomicU64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SlugRec {
    key_hash: String,
    last_seen: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StateFile {
    version: u32,
    slugs: HashMap<String, SlugRec>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// v2(`{version, slugs:{slug:{key_hash,last_seen}}}`) 와 v1(slug→키 해시 평면 맵)을 둘 다 읽는다.
/// 읽을 수 없는 파일은 옆으로 치워 두고 빈 채로 뜬다 — 조용히 덮어쓰면 묶음이 흔적 없이 사라진다.
fn load_state(p: &std::path::Path, now: u64) -> HashMap<String, SlugRec> {
    let Ok(raw) = std::fs::read_to_string(p) else {
        return HashMap::new();
    };
    let slugs = if let Ok(v2) = serde_json::from_str::<StateFile>(&raw) {
        v2.slugs
    } else if let Ok(v1) = serde_json::from_str::<HashMap<String, String>>(&raw) {
        v1.into_iter()
            .map(|(slug, key_hash)| (slug, SlugRec { key_hash, last_seen: now }))
            .collect()
    } else {
        let aside = p.with_extension(format!("json.corrupt-{now}"));
        let _ = std::fs::rename(p, &aside);
        eprintln!("[gateway] 상태 파일을 못 읽어 {} 로 치웠어요", aside.display());
        return HashMap::new();
    };
    slugs
        .into_iter()
        .filter(|(_, rec)| now.saturating_sub(rec.last_seen) < SLUG_RETENTION_SECS)
        .collect()
}

impl Gate {
    pub fn new(state_path: Option<PathBuf>) -> Self {
        let keys = state_path
            .as_deref()
            .map(|p| load_state(p, now_secs()))
            .unwrap_or_default();
        Self {
            by_slug: Arc::new(Mutex::new(HashMap::new())),
            keys: Arc::new(Mutex::new(keys)),
            conns: Arc::new(Mutex::new(HashMap::new())),
            state_path,
            seq: Arc::new(AtomicU64::new(1)),
        }
    }

    /// 임시 파일에 쓰고 fsync 뒤 이름을 바꾼다(0600) — 쓰다 죽어도 반쪽 파일이 남지 않는다.
    fn persist(&self) {
        let Some(p) = &self.state_path else { return };
        let body = match self.keys.lock() {
            Ok(k) => serde_json::to_string_pretty(&StateFile { version: 2, slugs: k.clone() }).unwrap_or_default(),
            Err(_) => return,
        };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = p.with_extension("json.tmp");
        let written = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, p)
        })();
        if let Err(e) = written {
            eprintln!("[gateway] 상태 파일을 못 썼어요: {e}");
        }
    }

    /// 이 키가 이 slug 를 써도 되나 — 처음이면 묶고, 아니면 같은 키여야 한다.
    fn claim(&self, slug: &str, key_hash: &str) -> bool {
        let mut k = self.keys.lock().unwrap();
        let now = now_secs();
        match k.get_mut(slug) {
            Some(rec) if rec.key_hash == key_hash => {
                rec.last_seen = now;
                true
            }
            Some(_) => false,
            None => {
                k.insert(slug.to_string(), SlugRec { key_hash: key_hash.to_string(), last_seen: now });
                true
            }
        }
    }

    /// 새 업링크를 받아도 되나 — 전체·키당 상한을 넘으면 거절한다.
    fn admit(&self, conn: u64, key_hash: &str) -> Result<(), &'static str> {
        let mut conns = self.conns.lock().unwrap();
        if conns.len() >= MAX_UPLINKS {
            return Err("관문에 붙은 기계가 너무 많아요");
        }
        if conns.values().filter(|h| *h == key_hash).count() >= MAX_UPLINKS_PER_KEY {
            return Err("같은 열쇠로 붙은 연결이 너무 많아요");
        }
        conns.insert(conn, key_hash.to_string());
        Ok(())
    }

    /// 지금 붙어 있는 주소 수 — 상태 창구용.
    pub fn live(&self) -> usize {
        self.by_slug.lock().map(|m| m.len()).unwrap_or(0)
    }
}

fn key_hash(key: &str) -> String {
    use sha2::Digest;
    let d = sha2::Sha256::digest(key.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn router(gate: Gate) -> Router {
    Router::new()
        .route("/relay/uplink", get(uplink_ws))
        .route("/u/{slug}", any(need_slash))
        .route("/u/{slug}/", any(proxy_root))
        .route("/u/{slug}/{*rest}", any(proxy))
        .with_state(gate)
}

async fn uplink_ws(State(gate): State<Gate>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |s| uplink_run(gate, s))
}

fn parse_hello(
    v: &serde_json::Value,
) -> Option<(String, Vec<String>, String, Option<String>, Vec<String>)> {
    if v.get("t")?.as_str()? != "hello" {
        return None;
    }
    let key = v.get("key")?.as_str()?.to_string();
    if key.len() < 16 || key.len() > 200 {
        return None;
    }
    let slugs: Vec<String> = v
        .get("slugs")?
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str())
        .filter(|s| crate::mobile::valid_slug(s))
        .take(MAX_SLUGS_PER_HELLO)
        .map(str::to_string)
        .collect();
    let machine: String = v
        .get("machine")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    let machine_id = v
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
    let mut aliases: Vec<String> = v
        .get("machine_aliases")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.chars().count() <= 80
                && !value.chars().any(char::is_control)
        })
        .map(str::to_string)
        .collect();
    aliases.push(machine.clone());
    aliases.sort();
    aliases.dedup();
    Some((key, slugs, machine, machine_id, aliases))
}

/// 연결 `conn` 만 그 slug 에서 뗀다 — 같은 slug 의 다른 살아 있는 연결은 남는다.
fn drop_conn(map: &mut HashMap<String, Vec<Arc<Uplink>>>, slug: &str, conn: u64) {
    if let Some(v) = map.get_mut(slug) {
        v.retain(|u| u.conn != conn);
        if v.is_empty() {
            map.remove(slug);
        }
    }
}

async fn uplink_run(gate: Gate, socket: WebSocket) {
    let (mut tx, mut rx) = socket.split();
    let first = tokio::time::timeout(Duration::from_secs(10), rx.next()).await;
    let hello = match first {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<serde_json::Value>(t.as_str()).ok(),
        _ => None,
    };
    let Some((key, slugs, machine, machine_id, aliases)) = hello.as_ref().and_then(parse_hello)
    else {
        let _ = tx
            .send(Message::Text(r#"{"t":"err","error":"hello 가 없거나 이상해요"}"#.into()))
            .await;
        return;
    };
    let hash = key_hash(&key);
    let conn = gate.seq.fetch_add(1, Ordering::Relaxed);
    if let Err(why) = gate.admit(conn, &hash) {
        let _ = tx
            .send(Message::Text(serde_json::json!({ "t": "err", "error": why }).to_string().into()))
            .await;
        return;
    }
    // 옛 앱은 stable id를 안 보낸다. 연결별 id로 두면 같은 표시 이름이 둘 뜬 순간
    // 모호함으로 닫히고, 이름만 보고 임의의 기계를 고르는 것보다 안전하다.
    let machine_id = machine_id.unwrap_or_else(|| format!("legacy-{conn}"));
    let (wtx, mut wrx) = mpsc::channel::<Message>(256);
    let up = Arc::new(Uplink {
        conn,
        machine: machine.clone(),
        machine_id,
        aliases,
        last_seen: Mutex::new(Instant::now()),
        tx: wtx.clone(),
        streams: Mutex::new(HashMap::new()),
        next: AtomicU32::new(1),
    });
    let apply = |slugs: &[String]| -> (Vec<String>, Vec<String>) {
        let mut acc = Vec::new();
        let mut rej = Vec::new();
        for s in slugs {
            if gate.claim(s, &hash) {
                let mut map = gate.by_slug.lock().unwrap();
                let v = map.entry(s.clone()).or_default();
                if !v.iter().any(|u| u.conn == conn) {
                    v.push(up.clone());
                }
                acc.push(s.clone());
            } else {
                rej.push(s.clone());
            }
        }
        gate.persist();
        (acc, rej)
    };
    let (acc, rej) = apply(&slugs);
    eprintln!(
        "[gateway] {machine} 붙음(#{conn}) — 주소 {}개{}",
        acc.len(),
        if rej.is_empty() { String::new() } else { format!(", 거절 {}개(다른 기계 소유)", rej.len()) }
    );
    let _ = tx
        .send(Message::Text(
            serde_json::json!({ "t": "ok", "accepted": acc, "rejected": rej }).to_string().into(),
        ))
        .await;
    let writer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.tick().await;
        loop {
            tokio::select! {
                m = wrx.recv() => match m {
                    Some(m) => if tx.send(m).await.is_err() { break },
                    None => break,
                },
                _ = tick.tick() => if tx.send(Message::Ping(Vec::new().into())).await.is_err() { break },
            }
        }
    });
    let mut mine: Vec<String> = acc;
    while let Some(m) = rx.next().await {
        match m {
            Ok(Message::Binary(b)) => {
                up.touch();
                let Some((kind, id, payload)) = decode(&b) else { continue };
                let s = up.streams.lock().unwrap().get(&id).cloned();
                if let Some(s) = s {
                    // 기다리지 않는다 — 폰 하나가 안 읽으면 이 기계의 다른 요청이 전부 선다.
                    // 밀린 스트림은 떼어 내고 앱에 CLOSE 로 알린다(떼어 낸 뒤라 guard 는 안 보낸다).
                    match s.try_send((kind, payload.to_vec())) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            up.streams.lock().unwrap().remove(&id);
                            let _ = wtx.try_send(Message::Binary(encode(CLOSE, id, b"overflow").into()));
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            up.streams.lock().unwrap().remove(&id);
                        }
                    }
                }
            }
            Ok(Message::Text(t)) => {
                up.touch();
                // hello 를 다시 보내면 slug 목록 갱신(유저가 늘었다).
                if let Some((k2, slugs2, _, _, _)) = serde_json::from_str::<serde_json::Value>(t.as_str())
                    .ok()
                    .as_ref()
                    .and_then(parse_hello)
                {
                    if key_hash(&k2) != hash {
                        continue;
                    }
                    let (acc2, rej2) = apply(&slugs2);
                    // 빠진 slug(유저 삭제)는 이 연결에서 뗀다. 잠금은 블록 안에서만 —
                    // 아래 await 를 넘기면 이 future 가 Send 가 아니게 된다.
                    {
                        let mut map = gate.by_slug.lock().unwrap();
                        for old in mine.iter().filter(|s| !acc2.contains(s)) {
                            drop_conn(&mut map, old, conn);
                        }
                    }
                    mine = acc2.clone();
                    let _ = wtx
                        .send(Message::Text(
                            serde_json::json!({ "t": "ok", "accepted": acc2, "rejected": rej2 }).to_string().into(),
                        ))
                        .await;
                }
            }
            Ok(Message::Ping(p)) => {
                up.touch();
                let _ = wtx.send(Message::Pong(p)).await;
            }
            Ok(Message::Pong(_)) => up.touch(),
            Ok(Message::Close(_)) | Err(_) => break,
        }
    }
    writer.abort();
    // 내 slug 만 뗀다 — 그 사이 같은 slug 로 새 연결이 붙었으면 그건 남긴다.
    {
        let mut map = gate.by_slug.lock().unwrap();
        for s in &mine {
            drop_conn(&mut map, s, conn);
        }
    }
    gate.conns.lock().unwrap().remove(&conn);
    up.streams.lock().unwrap().clear();
    eprintln!("[gateway] {machine} 떨어짐(#{conn})");
}

fn offline_page(slug_ok: bool) -> axum::response::Response {
    let msg = if slug_ok {
        "이 주소의 카사텀이 지금 안 붙어 있어요. 그 기계에서 앱이 켜져 있고 우하단 「● 바깥」이 켜져 있는지 봐 주세요."
    } else {
        "그런 주소가 없어요."
    };
    let html = format!(
        "<!doctype html><html lang=ko><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1,viewport-fit=cover\"><title>kasaterm</title>\
         <body style=\"margin:0;min-height:100dvh;display:flex;align-items:center;justify-content:center;background:#12161c;color:#c8d0d9;font:16px/1.6 -apple-system,system-ui,sans-serif;padding:24px;box-sizing:border-box\">\
         <div style=\"max-width:360px\"><b style=\"display:block;margin-bottom:8px\">kasaterm</b>{msg}</div></body></html>"
    );
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

async fn need_slash(AxPath(slug): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    axum::response::Redirect::temporary(&format!("/u/{slug}/{q}")).into_response()
}

async fn proxy_root(State(gate): State<Gate>, AxPath(slug): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    forward(gate, slug, String::new(), req).await
}

async fn proxy(State(gate): State<Gate>, AxPath((slug, rest)): AxPath<(String, String)>, req: axum::extract::Request) -> axum::response::Response {
    forward(gate, slug, rest, req).await
}

/// 스트림 하나의 수명 — 떨어질 때 관문 표에서 빠지고 앱에 CLOSE 를 알린다.
struct StreamGuard {
    up: Arc<Uplink>,
    id: u32,
}
impl Drop for StreamGuard {
    fn drop(&mut self) {
        if self.up.streams.lock().unwrap().remove(&self.id).is_some() {
            let _ = self.up.tx.try_send(Message::Binary(encode(CLOSE, self.id, b"").into()));
        }
    }
}

/// 요청 경로에서 `/u/<slug>/` 뒤의 **원문**(퍼센트 인코딩 그대로). 추출기가 준 `rest` 는
/// 이미 디코딩돼 있어, 그대로 넘기면 `%2e%2e` 가 앱에서 `..` 로 조립된다.
fn raw_rest<'a>(path: &'a str, slug: &str) -> &'a str {
    path.strip_prefix(crate::mobile::PREFIX)
        .and_then(|p| p.strip_prefix(slug))
        .map(|p| p.strip_prefix('/').unwrap_or(p))
        .unwrap_or("")
}

/// 폰이 실어 온 헤더 중 앱으로 넘기지 않는 것. 쿠키·Origin 류는 로컬 관문을 헷갈리게
/// 하고, 토큰류는 **주소 말고 다른 자격**을 관문 너머로 들이게 된다.
fn drop_request_header(k: &str) -> bool {
    skip_header(k)
        || crate::uplink::is_internal_header(k)
        || matches!(
            k,
            "cookie"
                | "origin"
                | "referer"
                | "authorization"
                | "x-kasa-token"
                | "x-forwarded-for"
                | "x-forwarded-proto"
                | "x-forwarded-host"
                | "cf-connecting-ip"
                | "cf-ray"
                | "cf-visitor"
                | "cdn-loop"
        )
        || k.starts_with("sec-")
}

async fn forward(gate: Gate, slug: String, rest: String, req: axum::extract::Request) -> axum::response::Response {
    if !crate::mobile::valid_slug(&slug) {
        return offline_page(false);
    }
    let raw = raw_rest(req.uri().path(), &slug).to_string();
    if !safe_path(&raw) {
        return (StatusCode::BAD_REQUEST, "bad path").into_response();
    }
    let uplinks = gate
        .by_slug
        .lock()
        .ok()
        .and_then(|map| map.get(&slug).cloned())
        .unwrap_or_default();
    let candidates = candidates_of(&uplinks);
    let requested_machine = machine_route(&rest).map(|(machine, _)| machine);
    let route = pick_route(&candidates, requested_machine);
    if route == RoutePick::Ambiguous {
        return (
            StatusCode::CONFLICT,
            "같은 이름의 기계가 둘이라 어느 쪽인지 고를 수 없어요",
        )
            .into_response();
    }
    let index = match route {
        RoutePick::Machine(index) | RoutePick::Fallback(index) => index,
        RoutePick::Missing | RoutePick::Ambiguous => {
        // 한 번도 등록된 적 없는 slug 는 「없는 주소」, 등록됐다 떨어진 slug 는 「안 붙어 있음」
        // — 폰에서 할 일이 다르다(주소를 다시 받기 vs 그 기계 앱 켜기).
        let known = gate.keys.lock().map(|k| k.contains_key(&slug)).unwrap_or(false);
        return offline_page(known);
        }
    };
    let up = uplinks[index].clone();
    // 기계 고르기는 디코딩된 이름으로 하고, 넘기는 건 원문 — `m/<기계>/` 두 조각만 벗긴다.
    let routed_rest = if matches!(route, RoutePick::Machine(_)) {
        raw.splitn(3, '/').nth(2).unwrap_or("")
    } else {
        raw.as_str()
    };
    if up.streams.lock().unwrap().len() >= MAX_STREAMS_PER_UPLINK {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many streams").into_response();
    }
    let live_machines = live_machines(&candidates);
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let path = format!("/{routed_rest}{query}");
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let headers: Vec<serde_json::Value> = req
        .headers()
        .iter()
        .filter(|(k, _)| !drop_request_header(k.as_str()))
        .filter_map(|(k, v)| Some(serde_json::json!([k.as_str(), v.to_str().ok()?])))
        .collect();
    let id = up.next.fetch_add(1, Ordering::Relaxed);
    let (stx, mut srx) = mpsc::channel::<Frame>(STREAM_QUEUE);
    up.streams.lock().unwrap().insert(id, stx);
    let guard = StreamGuard { up: up.clone(), id };
    let open = serde_json::json!({
        "slug": slug, "method": req.method().as_str(), "path": path, "headers": headers,
        "machines": live_machines, "ws": is_ws,
    })
    .to_string();
    if up.tx.send(Message::Binary(encode(OPEN, id, open.as_bytes()).into())).await.is_err() {
        return offline_page(true);
    }
    if is_ws {
        use axum::extract::FromRequestParts as _;
        let (mut parts, _body) = req.into_parts();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(w) => w,
            Err(e) => return e.into_response(),
        };
        return ws.on_upgrade(move |sock| ws_pipe(guard, srx, sock)).into_response();
    }
    // HTTP 바디는 받는 대로 흘려 보낸다 — 통째 모으면 요청 하나가 관문 메모리를 64MB 씩 문다.
    // 상한은 흘린 바이트로 센다. 넘으면 guard 가 떨어지며 앱에 CLOSE 가 간다.
    let (_parts, body) = req.into_parts();
    let mut body = body.into_data_stream();
    let mut sent = 0usize;
    while let Some(chunk) = body.next().await {
        let Ok(chunk) = chunk else {
            return (StatusCode::BAD_REQUEST, "body read failed").into_response();
        };
        sent += chunk.len();
        if sent > MAX_BODY {
            return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
        }
        for part in chunk.chunks(crate::uplink::CHUNK) {
            if up.tx.send(Message::Binary(encode(BODY, id, part).into())).await.is_err() {
                return offline_page(true);
            }
        }
    }
    if up.tx.send(Message::Binary(encode(END, id, b"").into())).await.is_err() {
        return offline_page(true);
    }
    // 앱의 답 머리 — 30초 안에 안 오면 그 기계가 멈춘 것.
    let head = loop {
        match tokio::time::timeout(Duration::from_secs(30), srx.recv()).await {
            Ok(Some((HEAD, p))) => break serde_json::from_slice::<serde_json::Value>(&p).ok(),
            Ok(Some((CLOSE, _))) | Ok(None) => break None,
            Ok(Some(_)) => continue,
            Err(_) => break None,
        }
    };
    let Some(head) = head else {
        return (StatusCode::BAD_GATEWAY, format!("{} 이(가) 답하지 않았어요", up.machine)).into_response();
    };
    let status = head.get("status").and_then(|s| s.as_u64()).unwrap_or(502) as u16;
    let mut out = axum::response::Response::builder().status(status);
    if let Some(hs) = head.get("headers").and_then(|h| h.as_array()) {
        for kv in hs {
            if let (Some(k), Some(v)) = (kv.get(0).and_then(|x| x.as_str()), kv.get(1).and_then(|x| x.as_str())) {
                // 앱이 심는 쿠키는 그 기계의 원격 토큰이다 — 주소만 아는 폰에 내주면 안 된다.
                if !skip_header(k) && !k.eq_ignore_ascii_case("set-cookie") {
                    out = out.header(k, v);
                }
            }
        }
    }
    let stream = async_stream(srx, guard);
    out.body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
}

/// BODY 조각을 응답 스트림으로 — END·CLOSE 에서 끝난다. guard 는 스트림과 수명을 같이한다.
fn async_stream(
    srx: mpsc::Receiver<Frame>,
    guard: StreamGuard,
) -> impl futures_util::Stream<Item = Result<Vec<u8>, std::io::Error>> {
    futures_util::stream::unfold((srx, guard, false), |(mut srx, guard, done)| async move {
        if done {
            return None;
        }
        // 이 스트림이 버려지면 guard 도 같이 떨어져 CLOSE 가 나간다.
        loop {
            match srx.recv().await {
                Some((BODY, p)) => return Some((Ok(p), (srx, guard, false))),
                Some((END, _)) | Some((CLOSE, _)) | None => return None,
                Some(_) => continue,
            }
        }
    })
}

async fn ws_pipe(guard: StreamGuard, mut srx: mpsc::Receiver<Frame>, sock: WebSocket) {
    let up = guard.up.clone();
    let id = guard.id;
    let (mut ctx, mut crx) = sock.split();
    // 앱이 로컬 WS 에 붙었는지(101) 먼저.
    let ok = loop {
        match tokio::time::timeout(Duration::from_secs(20), srx.recv()).await {
            Ok(Some((HEAD, p))) => {
                let v: serde_json::Value = serde_json::from_slice(&p).unwrap_or_default();
                break v.get("status").and_then(|s| s.as_u64()) == Some(101);
            }
            Ok(Some((CLOSE, _))) | Ok(None) | Err(_) => break false,
            Ok(Some(_)) => continue,
        }
    };
    if !ok {
        let _ = ctx
            .send(Message::Text(serde_json::json!({ "t": "gone", "why": "machine ws failed" }).to_string().into()))
            .await;
        return;
    }
    let send = |kind: u8, payload: Vec<u8>| {
        let tx = up.tx.clone();
        async move { tx.send(Message::Binary(encode(kind, id, &payload).into())).await.is_ok() }
    };
    loop {
        tokio::select! {
            m = crx.next() => match m {
                Some(Ok(Message::Text(t))) => if !send(WS_TEXT, t.as_str().as_bytes().to_vec()).await { break },
                Some(Ok(Message::Binary(b))) => if !send(WS_BIN, b.to_vec()).await { break },
                Some(Ok(Message::Ping(p))) => if !send(WS_PING, p.to_vec()).await { break },
                Some(Ok(Message::Pong(p))) => if !send(WS_PONG, p.to_vec()).await { break },
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
            },
            f = srx.recv() => match f {
                Some((WS_TEXT, p)) => {
                    let s = String::from_utf8_lossy(&p).to_string();
                    if ctx.send(Message::Text(s.into())).await.is_err() { break }
                }
                Some((WS_BIN, p)) => if ctx.send(Message::Binary(p.into())).await.is_err() { break },
                Some((WS_PING, p)) => if ctx.send(Message::Ping(p.into())).await.is_err() { break },
                Some((WS_PONG, p)) => if ctx.send(Message::Pong(p.into())).await.is_err() { break },
                Some((CLOSE, _)) | None => break,
                Some(_) => {}
            },
        }
    }
    let _ = ctx.close().await;
    drop(guard);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_uplink(machine: &str, machine_id: &str, age: Duration) -> Arc<Uplink> {
        let (tx, _rx) = mpsc::channel(1);
        Arc::new(Uplink {
            conn: 1,
            machine: machine.to_string(),
            machine_id: machine_id.to_string(),
            aliases: vec![machine.to_string()],
            last_seen: Mutex::new(Instant::now() - age),
            tx,
            streams: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
        })
    }

    #[test]
    fn slug_belongs_to_first_key() {
        let g = Gate::new(None);
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        assert!(!g.claim("abcdefghijklmnopqrstuvwxy", "h2"));
        assert!(g.claim("zyxwvutsrqponmlkjihgfedcb", "h2"));
    }

    #[test]
    fn hello_needs_key_and_valid_slugs() {
        let v = serde_json::json!({
            "t":"hello", "key":"0123456789abcdef0123",
            "slugs":["abcdefghijklmnopqrstuvwxy","BAD","short"],
            "machine":"맥북", "machine_id":"machine-macbook-1"
        });
        let (_, slugs, m, id, aliases) = parse_hello(&v).unwrap();
        assert_eq!(slugs, vec!["abcdefghijklmnopqrstuvwxy".to_string()]);
        assert_eq!(m, "맥북");
        assert_eq!(id.as_deref(), Some("machine-macbook-1"));
        assert!(aliases.contains(&"맥북".to_string()));
        assert!(parse_hello(&serde_json::json!({"t":"hello","key":"short","slugs":[]})).is_none());
        assert!(parse_hello(&serde_json::json!({"t":"nope"})).is_none());
    }

    #[test]
    fn live_machine_route_bypasses_direct_for_http_and_ws_paths() {
        let candidates = [
            Candidate { index: 0, machine: "맥북", machine_id: "book-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&candidates, Some("맥북")), RoutePick::Machine(0));
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Machine(1));
        assert_eq!(machine_route("m/미니/term/panes"), Some(("미니", "term/panes")));
        assert_eq!(machine_route("m/미니/term/ws"), Some(("미니", "term/ws")));
        assert_eq!(live_machines(&candidates).len(), 2);
    }

    #[test]
    fn missing_target_preserves_the_existing_direct_fallback() {
        let candidates = [Candidate {
            index: 0,
            machine: "맥북",
            machine_id: "book-1",
            aliases: &[],
            fresh: true,
        }];
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Fallback(0));
        assert_eq!(pick_route(&candidates, None), RoutePick::Fallback(0));
        assert_eq!(pick_route(&candidates, Some("~unknown-id")), RoutePick::Missing);
    }

    #[test]
    fn display_label_can_route_by_exact_alias_or_stable_id() {
        let aliases = vec!["맥미니".to_string(), "nachoneko".to_string()];
        let candidates = [Candidate {
            index: 0,
            machine: "nachoneko",
            machine_id: "stable-mini-1",
            aliases: &aliases,
            fresh: true,
        }];
        assert_eq!(pick_route(&candidates, Some("맥미니")), RoutePick::Machine(0));
        assert_eq!(
            pick_route(&candidates, Some("~stable-mini-1")),
            RoutePick::Machine(0)
        );
    }

    #[test]
    fn stale_and_ambiguous_routes_fail_closed() {
        let stale = [Candidate {
            index: 0,
            machine: "미니",
            machine_id: "mini-1",
            aliases: &[],
            fresh: false,
        }];
        assert_eq!(pick_route(&stale, Some("미니")), RoutePick::Missing);
        assert!(live_machines(&stale).is_empty());

        let ambiguous = [
            Candidate { index: 0, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "other-mini", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&ambiguous, Some("미니")), RoutePick::Ambiguous);
        assert_eq!(live_machines(&ambiguous).len(), 2);

        let restarted = [
            Candidate { index: 0, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
            Candidate { index: 1, machine: "미니", machine_id: "mini-1", aliases: &[], fresh: true },
        ];
        assert_eq!(pick_route(&restarted, Some("미니")), RoutePick::Machine(1));
        assert_eq!(live_machines(&restarted).len(), 1);
    }

    #[test]
    fn actual_uplink_freshness_expires_without_messages_or_pong() {
        let live = mock_uplink("맥북", "book-1", Duration::from_secs(1));
        let stale = mock_uplink("미니", "mini-1", UPLINK_STALE_AFTER);
        let uplinks = vec![live, stale];
        let candidates = candidates_of(&uplinks);
        assert_eq!(pick_route(&candidates, Some("맥북")), RoutePick::Machine(0));
        assert_eq!(pick_route(&candidates, Some("미니")), RoutePick::Fallback(0));
        assert_eq!(live_machines(&candidates).len(), 1);
    }

    #[test]
    fn uplinks_are_capped_per_key() {
        let g = Gate::new(None);
        for conn in 0..MAX_UPLINKS_PER_KEY as u64 {
            assert!(g.admit(conn, "k1").is_ok());
        }
        assert!(g.admit(100, "k1").is_err());
        assert!(g.admit(101, "k2").is_ok());
    }

    #[test]
    fn raw_rest_keeps_percent_encoding() {
        let slug = "abcdefghijklmnopqrstuvwxy";
        assert_eq!(raw_rest(&format!("/u/{slug}/%2e%2e/x"), slug), "%2e%2e/x");
        assert_eq!(raw_rest(&format!("/u/{slug}/"), slug), "");
        assert_eq!(raw_rest(&format!("/u/{slug}/m/%EB%AF%B8/term"), slug), "m/%EB%AF%B8/term");
        assert!(!safe_path(raw_rest(&format!("/u/{slug}/%2e%2e/x"), slug)));
    }

    #[test]
    fn state_reads_v1_writes_v2_0600_and_forgets_stale_slugs() {
        let dir = std::env::temp_dir().join(format!("kasa-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("relay-state.json");
        std::fs::write(&p, r#"{"abcdefghijklmnopqrstuvwxy":"h1"}"#).unwrap();
        let g = Gate::new(Some(p.clone()));
        assert!(!g.claim("abcdefghijklmnopqrstuvwxy", "h2"), "v1 묶음을 잃었다");
        g.persist();
        let v2: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v2["version"], 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let old = now_secs() - SLUG_RETENTION_SECS - 1;
        let stale = serde_json::json!({ "version": 2, "slugs": {
            "zyxwvutsrqponmlkjihgfedcb": { "key_hash": "h9", "last_seen": old }
        }});
        std::fs::write(&p, stale.to_string()).unwrap();
        assert!(Gate::new(Some(p.clone())).claim("zyxwvutsrqponmlkjihgfedcb", "h1"), "오래된 묶음이 남았다");
        std::fs::write(&p, "{broken").unwrap();
        assert!(Gate::new(Some(p.clone())).keys.lock().unwrap().is_empty());
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("corrupt")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 관문 ↔ 가짜 업링크 종단 시험 ─────────────────────────────────────────────

    use tokio_tungstenite::tungstenite::Message as TM;

    const SLUG: &str = "abcdefghijklmnopqrstuvwxy";

    struct FakeUplink {
        opens: Arc<Mutex<Vec<(u32, serde_json::Value)>>>,
        closes: Arc<Mutex<Vec<(u32, Vec<u8>)>>>,
    }

    /// 관문을 띄우고 가짜 앱 하나를 업링크로 붙인다. `answer` 는 OPEN 마다 불려 답 프레임을 쓴다.
    async fn relay_with_uplink(
        answer: impl Fn(u32, &serde_json::Value, mpsc::UnboundedSender<Vec<u8>>) + Send + Sync + 'static,
    ) -> (std::net::SocketAddr, FakeUplink) {
        let app = crate::relay::router().merge(router(Gate::new(None)));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/relay/uplink")).await.unwrap();
        let (mut tx, mut rx) = ws.split();
        let hello = serde_json::json!({
            "t": "hello", "key": "0123456789abcdef0123", "slugs": [SLUG],
            "machine": "맥북", "machine_id": "machine-test-1",
        });
        tx.send(TM::Text(hello.to_string().into())).await.unwrap();
        let ok = rx.next().await.unwrap().unwrap();
        assert!(ok.to_text().unwrap().contains("\"ok\""), "{ok:?}");
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if tx.send(TM::Binary(frame.into())).await.is_err() {
                    break;
                }
            }
        });
        let fake = FakeUplink { opens: Default::default(), closes: Default::default() };
        let (opens, closes) = (fake.opens.clone(), fake.closes.clone());
        tokio::spawn(async move {
            while let Some(Ok(m)) = rx.next().await {
                let TM::Binary(b) = m else { continue };
                let Some((kind, id, payload)) = decode(&b) else { continue };
                match kind {
                    OPEN => {
                        let v: serde_json::Value = serde_json::from_slice(payload).unwrap();
                        opens.lock().unwrap().push((id, v.clone()));
                        answer(id, &v, out.clone());
                    }
                    CLOSE => closes.lock().unwrap().push((id, payload.to_vec())),
                    _ => {}
                }
            }
        });
        (addr, fake)
    }

    fn reply(out: &mpsc::UnboundedSender<Vec<u8>>, id: u32, head: serde_json::Value, body: &[u8]) {
        let _ = out.send(encode(HEAD, id, head.to_string().as_bytes()));
        if !body.is_empty() {
            let _ = out.send(encode(BODY, id, body));
        }
        let _ = out.send(encode(END, id, b""));
    }

    #[tokio::test]
    async fn dot_segments_are_refused_before_reaching_the_machine() {
        let (addr, fake) = relay_with_uplink(|id, _, out| reply(&out, id, serde_json::json!({"status":200,"headers":[]}), b"x")).await;
        let status = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            write!(s, "GET /u/{SLUG}/%2e%2e/%2e%2e/version HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").unwrap();
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf.lines().next().unwrap_or("").to_string()
        })
        .await
        .unwrap();
        assert!(status.contains(" 400 "), "{status}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(fake.opens.lock().unwrap().is_empty(), "막혔어야 할 요청이 기계에 닿았다");
    }

    #[tokio::test]
    async fn credentials_do_not_cross_the_gateway_either_way() {
        let (addr, fake) = relay_with_uplink(|id, _, out| {
            let head = serde_json::json!({"status":200,"headers":[["set-cookie","kasa_token=secret"],["x-ok","1"]]});
            reply(&out, id, head, b"hi");
        })
        .await;
        let res = reqwest::Client::new()
            .get(format!("http://{addr}/u/{SLUG}/hub"))
            .header("authorization", "Bearer phone")
            .header("x-kasa-token", "stolen")
            .header("cookie", "kasa_token=stolen")
            .send()
            .await
            .unwrap();
        assert!(res.headers().get("set-cookie").is_none(), "앱의 토큰 쿠키가 폰에 샜다");
        assert_eq!(res.headers().get("x-ok").unwrap(), "1");
        assert_eq!(res.text().await.unwrap(), "hi");
        let opens = fake.opens.lock().unwrap();
        let sent: Vec<String> = opens[0].1["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| kv[0].as_str().unwrap().to_string())
            .collect();
        for h in ["authorization", "x-kasa-token", "cookie"] {
            assert!(!sent.iter().any(|s| s == h), "{h} 가 기계로 넘어갔다: {sent:?}");
        }
    }

    #[tokio::test]
    async fn a_phone_that_stops_reading_does_not_stall_other_requests() {
        let (addr, fake) = relay_with_uplink(|id, open, out| {
            let head = serde_json::json!({"status":200,"headers":[]});
            if open["path"] == "/slow" {
                let _ = out.send(encode(HEAD, id, head.to_string().as_bytes()));
                let chunk = vec![7u8; crate::uplink::CHUNK];
                for _ in 0..(STREAM_QUEUE * 3) {
                    let _ = out.send(encode(BODY, id, &chunk));
                }
            } else {
                reply(&out, id, head, b"fast");
            }
        })
        .await;
        let client = reqwest::Client::new();
        // 머리만 받고 몸통은 안 읽는다 — 폰이 멈춘 것.
        let _slow = client.get(format!("http://{addr}/u/{SLUG}/slow")).send().await.unwrap();
        let fast = tokio::time::timeout(Duration::from_secs(3), async {
            client.get(format!("http://{addr}/u/{SLUG}/fast")).send().await.unwrap().text().await.unwrap()
        })
        .await
        .expect("멈춘 폰 하나가 같은 기계의 다른 요청을 막았다");
        assert_eq!(fast, "fast");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !fake.closes.lock().unwrap().iter().any(|(_, why)| why == b"overflow") {
            assert!(Instant::now() < deadline, "밀린 스트림을 끊지 않았다");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn state_round_trip() {
        let dir = std::env::temp_dir().join(format!("kasa-gate-{}", uuid::Uuid::new_v4()));
        let p = dir.join("relay-state.json");
        let g = Gate::new(Some(p.clone()));
        assert!(g.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        g.persist();
        let g2 = Gate::new(Some(p));
        assert!(!g2.claim("abcdefghijklmnopqrstuvwxy", "h2"));
        assert!(g2.claim("abcdefghijklmnopqrstuvwxy", "h1"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
