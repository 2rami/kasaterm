//! 허용 목록. 들어오는 연결은 기존 기기 채널로 배운 EndpointId 만 받는다.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use iroh::endpoint::{AfterHandshakeOutcome, Connection, EndpointHooks, Side, VarInt};
use iroh::EndpointId;

pub const REJECT_CODE: u32 = 403;

#[derive(Clone, Debug, Default)]
pub struct AllowList(Arc<RwLock<HashSet<EndpointId>>>);

impl AllowList {
    pub fn new(ids: impl IntoIterator<Item = EndpointId>) -> Self {
        Self(Arc::new(RwLock::new(ids.into_iter().collect())))
    }

    pub fn insert(&self, id: EndpointId) -> bool {
        self.0.write().unwrap_or_else(|e| e.into_inner()).insert(id)
    }

    pub fn remove(&self, id: &EndpointId) -> bool {
        self.0.write().unwrap_or_else(|e| e.into_inner()).remove(id)
    }

    pub fn contains(&self, id: &EndpointId) -> bool {
        self.0
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }

    /// 엔드포인트에 거는 훅. 핸드셰이크 직후, 어떤 프로토콜 처리기도 연결을 보기 전에 끊는다.
    pub fn hook(&self) -> AllowHook {
        AllowHook(self.clone())
    }
}

#[derive(Debug)]
pub struct AllowHook(AllowList);

impl EndpointHooks for AllowHook {
    async fn after_handshake<'a>(&'a self, conn: &'a Connection) -> AfterHandshakeOutcome {
        // 내가 건 연결은 상대를 골라서 건 것이다. 막는 것은 들어오는 쪽뿐.
        if conn.side() == Side::Client || self.0.contains(&conn.remote_id()) {
            return AfterHandshakeOutcome::Accept;
        }
        AfterHandshakeOutcome::Reject {
            error_code: VarInt::from_u32(REJECT_CODE),
            reason: b"kasanet: unknown endpoint".to_vec(),
        }
    }
}
