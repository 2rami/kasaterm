//! 배치 채널 — 거울 쪽. 보기 창이 있는 기계마다 `/term/layout/ws` 에 붙어 원본 방의 트리를
//! 받는다(원본 쪽은 `layout_feed`). 분할선 명령도 이 소켓으로 보내고, 원본이 그 순번까지
//! 적용했다고 확인하기 전에 온 배치는 옛 모습이라 내놓지 않는다 — 끄는 도중에 옛 비율로
//! 튀지 않는다. 채널이 없는 옛 판 원본이면 `is_live` 가 거짓이고 부른 쪽이 좌표 폴링으로
//! 버틴다.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub use crate::layout_feed::Room;

#[derive(Default)]
struct State {
    /// 이 연결로 배치를 한 번이라도 받았다.
    live: bool,
    rooms: Option<Arc<Vec<Room>>>,
    version: u64,
    /// 이 연결로 보낸 마지막 순번과, 원본이 적용했다고 확인한 순번.
    sent: u64,
    acked: u64,
}

/// `(합칠 열쇠, 보낼 글)` — 같은 분할선의 밀린 명령은 마지막만 보낸다.
type Op = (String, String);

struct Watch {
    state: Arc<Mutex<State>>,
    ops: mpsc::UnboundedSender<Op>,
    /// 보기 창이 사라진 때. 쪼개는 동안 새 칸이 아직 거울이 아니라 창이 잠깐 보기 창에서
    /// 빠지는데, 그때마다 끊었다 다시 붙지 않게 한참 뒤에 끊는다.
    unused_since: Option<std::time::Instant>,
}

const UNUSED_GRACE: Duration = Duration::from_secs(30);

fn watches() -> &'static Mutex<HashMap<String, Watch>> {
    static W: OnceLock<Mutex<HashMap<String, Watch>>> = OnceLock::new();
    W.get_or_init(Default::default)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

fn waker() -> &'static OnceLock<Box<dyn Fn() + Send + Sync>> {
    static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    &WAKE
}

/// 배치가 오면 GUI 루프를 깨울 손잡이. 안 깨우면 다음 타이머까지 화면이 옛 배치다.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = waker().set(Box::new(wake));
}

fn wake() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if let Some(wake) = waker().get() {
        wake();
    }
}

/// 받은 배치·연결 상태가 바뀔 때마다 오른다.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// 이 기계들을 지켜본다. 한참 빠져 있던 기계의 연결은 보낼 줄을 떨궈 끝낸다.
pub fn retain(bases: &[String]) {
    let now = std::time::Instant::now();
    let mut all = watches().lock().unwrap();
    all.retain(|base, watch| {
        if bases.contains(base) {
            watch.unused_since = None;
            return true;
        }
        now.duration_since(*watch.unused_since.get_or_insert(now)) < UNUSED_GRACE
    });
    for base in bases {
        if !all.contains_key(base) {
            all.insert(base.clone(), spawn(base.clone()));
        }
    }
}

/// 원본이 이 채널로 배치를 주고 있나.
pub fn is_live(base: &str) -> bool {
    watches().lock().unwrap().get(base).is_some_and(|w| w.state.lock().unwrap().live)
}

/// 보낸 명령이 다 적용된 뒤의 배치 `(원본 판 번호, 방들)`. 아직이면 None.
pub fn settled(base: &str) -> Option<(u64, Arc<Vec<Room>>)> {
    let all = watches().lock().unwrap();
    let state = all.get(base)?.state.lock().unwrap();
    if !state.live || state.acked < state.sent {
        return None;
    }
    Some((state.version, state.rooms.clone()?))
}

/// 원본 방들에 그 PTY 가 지금 앉아 있나. 채널이 없으면 모른다(None).
pub fn source_has(base: &str, pty: &str) -> Option<bool> {
    let all = watches().lock().unwrap();
    let state = all.get(base)?.state.lock().unwrap();
    let rooms = state.rooms.as_ref().filter(|_| state.live)?;
    Some(rooms.iter().any(|room| {
        room.tree.leaves().contains(&pty) || room.members.values().any(|ids| ids.iter().any(|id| id == pty))
    }))
}

/// 분할선 명령(`pairs`·`ratio`·`dir`)을 보낸다. 채널이 없으면 거짓 — 부른 쪽이 HTTP 로.
pub fn send_ratio(base: &str, params: &serde_json::Value) -> bool {
    let all = watches().lock().unwrap();
    let Some(watch) = all.get(base) else { return false };
    let mut state = watch.state.lock().unwrap();
    if !state.live {
        return false;
    }
    let mut op = params.clone();
    op["t"] = "ratio".into();
    op["seq"] = (state.sent + 1).into();
    let key = format!("{}|{}", params["pairs"], params["dir"]);
    if watch.ops.send((key, op.to_string())).is_err() {
        return false;
    }
    state.sent += 1;
    true
}

fn spawn(base: String) -> Watch {
    let state = Arc::new(Mutex::new(State::default()));
    let (tx, rx) = mpsc::unbounded_channel();
    let shared = state.clone();
    let _ = std::thread::Builder::new().name("layout-watch".into()).spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        rt.block_on(run(base, shared, rx));
    });
    Watch { state, ops: tx, unused_since: None }
}

fn url(base: &str) -> String {
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{base}")
    };
    let mut url = format!("{}/term/layout/ws", ws.trim_end_matches('/'));
    if let Some(token) = crate::remote::connection_auth_token(base) {
        url.push_str("?t=");
        url.push_str(&token);
    }
    url
}

/// 연결 밖에서 기다린다 — 그동안 온 명령은 버린다(채널이 없다고 이미 답했다). 보낼 줄이
/// 닫혔으면(이 기계를 더 안 본다) 거짓.
async fn idle(ops: &mut mpsc::UnboundedReceiver<Op>, wait: Duration) -> bool {
    let until = tokio::time::sleep(wait);
    tokio::pin!(until);
    loop {
        tokio::select! {
            _ = &mut until => return true,
            op = ops.recv() => if op.is_none() { return false },
        }
    }
}

/// 한 번에 받은 명령 묶음에서 같은 분할선의 앞선 것을 걷는다. 마지막 순번은 늘 남는다.
fn coalesce(batch: Vec<Op>) -> Vec<String> {
    let mut out: Vec<Op> = Vec::with_capacity(batch.len());
    for (key, text) in batch {
        out.retain(|(k, _)| k != &key);
        out.push((key, text));
    }
    out.into_iter().map(|(_, text)| text).collect()
}

fn on_frame(state: &Mutex<State>, text: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return false };
    if v.get("t").and_then(|t| t.as_str()) != Some("layout") {
        return false;
    }
    let Ok(rooms) = serde_json::from_value::<Vec<Room>>(v["rooms"].clone()) else { return false };
    let mut state = state.lock().unwrap();
    state.live = true;
    state.version = v["v"].as_u64().unwrap_or(0);
    state.acked = state.acked.max(v["ack"].as_u64().unwrap_or(0));
    state.rooms = Some(Arc::new(rooms));
    true
}

async fn run(base: String, state: Arc<Mutex<State>>, mut ops: mpsc::UnboundedReceiver<Op>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let connected = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(url(&base))).await;
        let ws = match connected {
            Ok(Ok((ws, _))) => ws,
            // 옛 판 원본 — 라우트가 없다. 갱신될 수도 있으니 가끔 다시 본다.
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(resp))) if resp.status().as_u16() == 404 => {
                if !idle(&mut ops, Duration::from_secs(60)).await { return; }
                continue;
            }
            _ => {
                if !idle(&mut ops, backoff).await { return; }
                backoff = (backoff * 2).min(Duration::from_secs(10));
                continue;
            }
        };
        backoff = Duration::from_secs(1);
        {
            let mut s = state.lock().unwrap();
            (s.sent, s.acked) = (0, 0);
        }
        let (mut tx, mut rx) = ws.split();
        // 원본은 20초마다 ping 한다. 그보다 한참 조용하면 죽은 연결(잠든 터널)이다.
        let mut heard = tokio::time::Instant::now();
        let mut check = tokio::time::interval(Duration::from_secs(15));
        let closed = loop {
            tokio::select! {
                m = rx.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        heard = tokio::time::Instant::now();
                        if on_frame(&state, t.as_str()) { wake(); }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break false,
                    Some(Ok(_)) => heard = tokio::time::Instant::now(),
                },
                op = ops.recv() => {
                    let Some(first) = op else { break true };
                    let mut batch = vec![first];
                    while let Ok(next) = ops.try_recv() { batch.push(next); }
                    let mut failed = false;
                    for text in coalesce(batch) {
                        if tx.send(Message::Text(text.into())).await.is_err() { failed = true; break; }
                    }
                    if failed { break false; }
                }
                _ = check.tick() => {
                    if heard.elapsed() > Duration::from_secs(50) { break false; }
                    // 받은 ping 의 pong 은 쓰기 때 나간다 — 쓸 일이 없으면 여기서 민다.
                    if tx.flush().await.is_err() { break false; }
                }
            }
        };
        {
            let mut s = state.lock().unwrap();
            (s.live, s.rooms, s.sent, s.acked) = (false, None, 0, 0);
        }
        while ops.try_recv().is_ok() {}
        wake();
        if closed || !idle(&mut ops, backoff).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesce_keeps_last_per_seam_and_the_latest_overall() {
        let op = |k: &str, seq: u32| (k.to_string(), format!("{k}{seq}"));
        let out = coalesce(vec![op("a", 1), op("b", 2), op("a", 3), op("b", 4), op("a", 5)]);
        assert_eq!(out, vec!["b4".to_string(), "a5".to_string()]);
    }

    #[test]
    fn frame_before_ack_is_not_settled() {
        let state = Mutex::new(State { sent: 3, ..Default::default() });
        let frame = |ack: u64| serde_json::json!({"t": "layout", "v": 9, "ack": ack, "rooms": []}).to_string();
        assert!(on_frame(&state, &frame(2)));
        {
            let s = state.lock().unwrap();
            assert!(s.live && s.acked < s.sent);
        }
        on_frame(&state, &frame(3));
        let s = state.lock().unwrap();
        assert_eq!((s.acked, s.version), (3, 9));
        assert!(!on_frame(&state, r#"{"t":"size"}"#));
    }

    #[test]
    fn url_maps_scheme_and_carries_no_token_without_link() {
        assert_eq!(url("http://127.0.0.1:18795"), "ws://127.0.0.1:18795/term/layout/ws");
        assert_eq!(url("https://gw.example/u/abc/"), "wss://gw.example/u/abc/term/layout/ws");
    }
}
