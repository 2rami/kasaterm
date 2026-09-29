//! 한 상대와의 QUIC 연결을 들고, 지금 길이 직통인지 지켜본다.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::fwd;

const WATCH_EVERY: Duration = Duration::from_millis(200);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_MIN: Duration = Duration::from_secs(1);
/// 상대가 아직 이쪽 id 를 못 배워 403 으로 끊는 동안에도 너무 늦게 다시 걸지 않게.
const RETRY_MAX: Duration = Duration::from_secs(15);
/// 허용 목록 거절(403)은 핸드셰이크가 끝난 뒤에 온다. 그 전에 직통으로 알리면 거절될 연결에 데이터를 싣는다.
const FIRST_LOOK: Duration = Duration::from_millis(500);
/// 이보다 짧게 살다 끊긴 연결은 실패로 친다 — 거절당하는 동안 1초마다 다시 거는 일이 없게.
const STABLE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LinkState {
    Down,
    /// 붙었지만 중계로만 간다. 데이터는 싣지 않는다 — n0 공용 중계는 ssh 길보다 느리다(docs/kasanet.md P0).
    Relay,
    Direct {
        rtt: Duration,
    },
}

impl LinkState {
    pub fn is_direct(&self) -> bool {
        matches!(self, LinkState::Direct { .. })
    }
}

#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}

struct Inner {
    id: EndpointId,
    addr: Mutex<EndpointAddr>,
    conn: Mutex<Option<Connection>>,
    state: watch::Sender<LinkState>,
    last_error: Mutex<Option<String>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().ok().and_then(Option::take) {
            task.abort();
        }
        if let Some(conn) = self.conn.get_mut().ok().and_then(Option::take) {
            conn.close(0u32.into(), b"kasanet: link dropped");
        }
    }
}

impl Link {
    /// 붙기를 뒤에서 계속 시도한다. 끊기면 다시 건다. tokio 런타임 안에서 부른다.
    pub fn start(endpoint: Endpoint, peer: EndpointAddr) -> Self {
        let inner = Arc::new(Inner {
            id: peer.id,
            addr: Mutex::new(peer),
            conn: Mutex::new(None),
            state: watch::channel(LinkState::Down).0,
            last_error: Mutex::new(None),
            task: Mutex::new(None),
        });
        let task = tokio::spawn(maintain(endpoint, Arc::downgrade(&inner)));
        *inner.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        Self { inner }
    }

    pub fn id(&self) -> EndpointId {
        self.inner.id
    }

    /// 상대 주소가 바뀌었으면(망을 옮김) 다음 연결부터 새 주소로 건다.
    pub fn set_addr(&self, peer: EndpointAddr) {
        if peer.id == self.inner.id {
            *self.inner.addr.lock().unwrap_or_else(|e| e.into_inner()) = peer;
        }
    }

    pub fn state(&self) -> LinkState {
        *self.inner.state.borrow()
    }

    /// 종류(끊김·중계·직통)가 바뀔 때만 깨운다. 왕복 값의 흔들림으로는 안 깨운다.
    pub fn subscribe(&self) -> watch::Receiver<LinkState> {
        self.inner.state.subscribe()
    }

    /// 마지막으로 못 붙은·끊긴 까닭. 붙어 있는 동안에도 지우지 않는다 — 「왜 가끔 ssh 로 가나」를 볼 때 쓴다.
    pub fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 직통일 때만 연결을 내준다.
    pub fn direct(&self) -> Option<Connection> {
        if !self.state().is_direct() {
            return None;
        }
        self.inner
            .conn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

fn observe(conn: &Connection) -> LinkState {
    if conn.close_reason().is_some() {
        return LinkState::Down;
    }
    let paths = conn.paths();
    match paths.iter().find(|p| p.is_selected()) {
        Some(p) if p.is_ip() => LinkState::Direct { rtt: p.rtt() },
        _ => LinkState::Relay,
    }
}

fn publish(inner: &Inner, next: LinkState) {
    inner.state.send_if_modified(|cur| {
        let kind_changed = std::mem::discriminant(cur) != std::mem::discriminant(&next);
        *cur = next;
        kind_changed
    });
}

async fn maintain(endpoint: Endpoint, weak: Weak<Inner>) {
    let mut wait = RETRY_MIN;
    loop {
        let Some(addr) = weak
            .upgrade()
            .map(|i| i.addr.lock().unwrap_or_else(|e| e.into_inner()).clone())
        else {
            return;
        };
        let conn =
            match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr, fwd::ALPN)).await {
                Ok(Ok(conn)) => conn,
                failed => {
                    let why = match failed {
                        Ok(Err(e)) => format!("연결 실패: {e:#}"),
                        _ => "연결 시간 초과".to_string(),
                    };
                    if let Some(inner) = weak.upgrade() {
                        *inner.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
                    }
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(RETRY_MAX);
                    continue;
                }
            };
        let connected_at = std::time::Instant::now();
        match weak.upgrade() {
            Some(inner) => {
                *inner.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone())
            }
            None => {
                conn.close(0u32.into(), b"kasanet: link dropped");
                return;
            }
        }
        tokio::time::sleep(FIRST_LOOK).await;
        loop {
            let Some(inner) = weak.upgrade() else {
                conn.close(0u32.into(), b"kasanet: link dropped");
                return;
            };
            let state = observe(&conn);
            if state == LinkState::Down {
                *inner.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *inner.last_error.lock().unwrap_or_else(|e| e.into_inner()) =
                    conn.close_reason().map(|r| format!("끊김: {r}"));
                publish(&inner, LinkState::Down);
                break;
            }
            publish(&inner, state);
            drop(inner);
            tokio::time::sleep(WATCH_EVERY).await;
        }
        wait = if connected_at.elapsed() >= STABLE {
            RETRY_MIN
        } else {
            (wait * 2).min(RETRY_MAX)
        };
        tokio::time::sleep(wait).await;
    }
}
