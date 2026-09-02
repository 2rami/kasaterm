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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

use crate::uplink::{decode, encode, skip_header, BODY, CLOSE, END, HEAD, OPEN, WS_BIN, WS_PING, WS_PONG, WS_TEXT};

type Frame = (u8, Vec<u8>);

struct Uplink {
    conn: u64,
    machine: String,
    tx: mpsc::Sender<Message>,
    streams: Mutex<HashMap<u32, mpsc::Sender<Frame>>>,
    next: AtomicU32,
}

#[derive(Clone)]
pub struct Gate {
    /// slug → 그 slug 를 알린 **살아 있는 연결들**(뒤가 최신). 하나만 두면 같은
    /// 기계에서 앱이 둘 뜬 경우(검증 리그·재시작 겹침)에 나중 것이 떠나며 먼저 것을
    /// 함께 떼어 버린다 — 2026-09-02 실측: 리그가 붙었다 떠나자 실앱 주소가 502.
    by_slug: Arc<Mutex<HashMap<String, Vec<Arc<Uplink>>>>>,
    /// slug → 키 해시. 처음 온 키가 주인이다.
    keys: Arc<Mutex<HashMap<String, String>>>,
    state_path: Option<PathBuf>,
    seq: Arc<AtomicU64>,
}

impl Gate {
    pub fn new(state_path: Option<PathBuf>) -> Self {
        let keys = state_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
            .unwrap_or_default();
        Self {
            by_slug: Arc::new(Mutex::new(HashMap::new())),
            keys: Arc::new(Mutex::new(keys)),
            state_path,
            seq: Arc::new(AtomicU64::new(1)),
        }
    }

    fn persist(&self) {
        let Some(p) = &self.state_path else { return };
        let body = match self.keys.lock() {
            Ok(k) => serde_json::to_string_pretty(&*k).unwrap_or_default(),
            Err(_) => return,
        };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, body);
    }

    /// 이 키가 이 slug 를 써도 되나 — 처음이면 묶고, 아니면 같은 키여야 한다.
    fn claim(&self, slug: &str, key_hash: &str) -> bool {
        let mut k = self.keys.lock().unwrap();
        match k.get(slug) {
            Some(h) => h == key_hash,
            None => {
                k.insert(slug.to_string(), key_hash.to_string());
                true
            }
        }
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

fn parse_hello(v: &serde_json::Value) -> Option<(String, Vec<String>, String)> {
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
        .map(str::to_string)
        .collect();
    let machine = v
        .get("machine")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .chars()
        .take(40)
        .collect();
    Some((key, slugs, machine))
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
    let Some((key, slugs, machine)) = hello.as_ref().and_then(parse_hello) else {
        let _ = tx
            .send(Message::Text(r#"{"t":"err","error":"hello 가 없거나 이상해요"}"#.into()))
            .await;
        return;
    };
    let hash = key_hash(&key);
    let conn = gate.seq.fetch_add(1, Ordering::Relaxed);
    let (wtx, mut wrx) = mpsc::channel::<Message>(256);
    let up = Arc::new(Uplink {
        conn,
        machine: machine.clone(),
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
                let Some((kind, id, payload)) = decode(&b) else { continue };
                let s = up.streams.lock().unwrap().get(&id).cloned();
                if let Some(s) = s {
                    if s.send((kind, payload.to_vec())).await.is_err() {
                        up.streams.lock().unwrap().remove(&id);
                    }
                }
            }
            Ok(Message::Text(t)) => {
                // hello 를 다시 보내면 slug 목록 갱신(유저가 늘었다).
                if let Some((k2, slugs2, _)) = serde_json::from_str::<serde_json::Value>(t.as_str())
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
                let _ = wtx.send(Message::Pong(p)).await;
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
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

async fn forward(gate: Gate, slug: String, rest: String, req: axum::extract::Request) -> axum::response::Response {
    if !crate::mobile::valid_slug(&slug) {
        return offline_page(false);
    }
    let up = gate.by_slug.lock().unwrap().get(&slug).and_then(|v| v.last().cloned());
    let Some(up) = up else {
        // 한 번도 등록된 적 없는 slug 는 「없는 주소」, 등록됐다 떨어진 slug 는 「안 붙어 있음」
        // — 폰에서 할 일이 다르다(주소를 다시 받기 vs 그 기계 앱 켜기).
        let known = gate.keys.lock().map(|k| k.contains_key(&slug)).unwrap_or(false);
        return offline_page(known);
    };
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let path = format!("/{rest}{query}");
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let headers: Vec<serde_json::Value> = req
        .headers()
        .iter()
        .filter(|(k, _)| {
            let k = k.as_str();
            !skip_header(k)
                && !matches!(k, "cookie" | "origin" | "referer" | "x-forwarded-for" | "x-forwarded-proto" | "x-forwarded-host" | "cf-connecting-ip" | "cf-ray" | "cf-visitor" | "cdn-loop")
                && !k.starts_with("sec-")
        })
        .filter_map(|(k, v)| Some(serde_json::json!([k.as_str(), v.to_str().ok()?])))
        .collect();
    let id = up.next.fetch_add(1, Ordering::Relaxed);
    let (stx, mut srx) = mpsc::channel::<Frame>(64);
    up.streams.lock().unwrap().insert(id, stx);
    let guard = StreamGuard { up: up.clone(), id };
    let open = serde_json::json!({
        "slug": slug, "method": req.method().as_str(), "path": path, "headers": headers, "ws": is_ws,
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
    // HTTP 바디를 통째 내려보낸다(폰 요청은 작다 — 업로드는 64MB 상한).
    let (_parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };
    for part in bytes.chunks(crate::uplink::CHUNK) {
        if up.tx.send(Message::Binary(encode(BODY, id, part).into())).await.is_err() {
            return offline_page(true);
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
                if !skip_header(k) {
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
        let v = serde_json::json!({"t":"hello","key":"0123456789abcdef0123","slugs":["abcdefghijklmnopqrstuvwxy","BAD","short"],"machine":"맥북"});
        let (_, slugs, m) = parse_hello(&v).unwrap();
        assert_eq!(slugs, vec!["abcdefghijklmnopqrstuvwxy".to_string()]);
        assert_eq!(m, "맥북");
        assert!(parse_hello(&serde_json::json!({"t":"hello","key":"short","slugs":[]})).is_none());
        assert!(parse_hello(&serde_json::json!({"t":"nope"})).is_none());
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
