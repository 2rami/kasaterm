//! 배치 채널 — 원본 쪽. 이 기계의 방마다 BSP 트리를 `/term/layout/ws` 로 바로 민다.
//!
//! 거울은 예전엔 `/term/panes` 의 칸 좌표를 몇 초마다 다시 읽어 트리를 **추측**했다. 좌표만
//! 으로는 어느 선부터 잘랐는지 알 수 없어 2×2 격자가 뒤집혔고, 읽는 데만 기계마다 요청
//! 넷·2초 스로틀·3~15초 억제가 얹혀 분할선이 한참 뒤에 따라왔다(2026-09-25). 이제 트리
//! 자체를 바뀌는 순간 보낸다. 거울의 분할선 명령도 같은 소켓으로 받아, 적용된 뒤에야
//! 그 명령의 순번을 확인해 준다 — 거울은 확인 전에 온 배치를 옛 모습으로 보고 버린다.
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use kasa_socket::backend::{Backend, SeamAxis};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

/// 방 하나. `members` 는 leaf 마다 그 자리에 든 PTY 번호(탭 포함) — 거울은 PTY 번호로
/// 붙어 있어서, leaf 번호만으로는 탭을 빼낸 자리를 못 짚는다.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub window: usize,
    pub tree: kasa_pty::PtyLayout,
    #[serde(default)]
    pub members: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Default)]
struct Snapshot {
    version: u64,
    rooms: Arc<Vec<Room>>,
    acks: HashMap<u64, u64>,
}

fn chan() -> &'static watch::Sender<Snapshot> {
    static CHAN: OnceLock<watch::Sender<Snapshot>> = OnceLock::new();
    CHAN.get_or_init(|| watch::channel(Snapshot::default()).0)
}

/// 지금 배치를 싣는다. 같으면 아무것도 안 나가니 매 루프 불러도 된다.
pub fn publish(rooms: Vec<Room>) {
    chan().send_if_modified(|snap| {
        if *snap.rooms == rooms {
            return false;
        }
        snap.version += 1;
        snap.rooms = Arc::new(rooms);
        true
    });
}

/// `client` 연결이 보낸 `seq` 번까지 적용됐다.
pub fn ack(client: u64, seq: u64) {
    chan().send_modify(|snap| {
        let slot = snap.acks.entry(client).or_default();
        *slot = (*slot).max(seq);
    });
}

/// 끊긴 연결의 확인을 걷는다. 남은 연결에 알릴 일은 아니다.
fn forget(client: u64) {
    chan().send_if_modified(|snap| {
        snap.acks.remove(&client);
        false
    });
}

fn frame(snap: &Snapshot, client: u64) -> String {
    serde_json::json!({
        "t": "layout",
        "v": snap.version,
        "ack": snap.acks.get(&client).copied().unwrap_or(0),
        "rooms": &*snap.rooms,
    })
    .to_string()
}

/// 거울이 보낸 명령 하나. 모르는 종류도 순번은 확인한다 — 안 하면 거울이 그 뒤 배치를
/// 영영 안 받는다.
fn apply(backend: &dyn Backend, client: u64, text: &str) {
    let Ok(op) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let Some(seq) = op.get("seq").and_then(|v| v.as_u64()) else { return };
    if op.get("t").and_then(|v| v.as_str()) == Some("ratio") {
        let pairs: Vec<(String, String)> = op.get("pairs").and_then(|v| v.as_array()).into_iter().flatten()
            .filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string())))
            .collect();
        let ratio = op.get("ratio").and_then(|v| v.as_f64());
        let axis = op.get("dir").and_then(|v| v.as_str()).and_then(SeamAxis::parse);
        if let (false, Some(ratio)) = (pairs.is_empty(), ratio) {
            if let Err(e) = backend.set_ratio_between(&pairs, ratio as f32, axis) {
                eprintln!("[layout-ws] 분할선 명령을 못 걸었다: {e:#}");
            }
        }
    }
    if backend.layout_barrier(client, seq).is_err() {
        ack(client, seq);
    }
}

pub(crate) async fn serve(mut socket: WebSocket, backend: Arc<dyn Backend>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let client = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut rx = chan().subscribe();
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    ping.tick().await;
    // 방은 그대로인데 남의 연결 확인만 바뀐 것은 다시 안 보낸다.
    let mut sent: Option<(u64, u64)> = None;
    loop {
        let out = {
            let snap = rx.borrow_and_update();
            let key = (snap.version, snap.acks.get(&client).copied().unwrap_or(0));
            (sent != Some(key)).then(|| {
                sent = Some(key);
                frame(&snap, client)
            })
        };
        if let Some(out) = out {
            if socket.send(Message::Text(out.into())).await.is_err() {
                break;
            }
        }
        tokio::select! {
            changed = rx.changed() => if changed.is_err() { break },
            m = socket.recv() => match m {
                Some(Ok(Message::Text(t))) => apply(&*backend, client, t.as_str()),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
            _ = ping.tick() => if socket.send(Message::Ping(Default::default())).await.is_err() { break },
        }
    }
    forget(client);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(ratio: f32) -> Room {
        Room {
            window: 0,
            tree: kasa_pty::PtyLayout::Split {
                dir: kasa_pty::SplitDir::Horizontal,
                ratio,
                a: Box::new(kasa_pty::PtyLayout::single("%1")),
                b: Box::new(kasa_pty::PtyLayout::single("%2")),
            },
            members: BTreeMap::new(),
        }
    }

    #[test]
    fn frame_carries_tree_and_only_this_clients_ack() {
        let snap = Snapshot {
            version: 7,
            rooms: Arc::new(vec![room(0.3)]),
            acks: HashMap::from([(1, 4), (2, 9)]),
        };
        let v: serde_json::Value = serde_json::from_str(&frame(&snap, 1)).unwrap();
        assert_eq!((v["v"].as_u64(), v["ack"].as_u64()), (Some(7), Some(4)));
        let rooms: Vec<Room> = serde_json::from_value(v["rooms"].clone()).unwrap();
        assert_eq!(rooms, vec![room(0.3)]);
        let other: serde_json::Value = serde_json::from_str(&frame(&snap, 3)).unwrap();
        assert_eq!(other["ack"].as_u64(), Some(0));
    }

    #[test]
    fn same_rooms_do_not_bump_version() {
        publish(vec![room(0.41)]);
        let before = chan().borrow().version;
        publish(vec![room(0.41)]);
        assert_eq!(chan().borrow().version, before);
        publish(vec![room(0.42)]);
        assert_eq!(chan().borrow().version, before + 1);
    }
}
