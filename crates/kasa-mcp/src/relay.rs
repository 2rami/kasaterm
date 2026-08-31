//! 사내 세션 소통 **중계 서버**(2단계) — 기계들이 서로 직접 터널을 파지 않고 한
//! 중계소에 등록하면, 중계소가 세션 목록(필터된 보드)을 모아 주고 메시지를 대상
//! 기계로 라우팅한다. 다른 계정끼리(3단계)의 신뢰 경계도 여기다.
//!
//! 이 모듈은 **라우팅·등록·필터된 보드** 셋만 한다. 부탁/지시 봉투는 이미 수신측
//! (`http::term_message_post` 의 `cross_session_content`)이 발신자 신원(from_person)
//! 유무로 씌우므로, 중계소는 그 신원을 **그대로 실어 나르기만** 하면 된다 — 정책을
//! 두 곳에 두지 않는다.
//!
//! 거노 결정(2026-09-01) 반영:
//! - 공유 보드(`GET /relay/sessions`)는 **방·제목·상태만** — sid·name·machine·
//!   account·status 만 내고 대화·비용은 애초에 안 받고 안 낸다.
//! - 다른 계정 발신은 **부탁으로만** — 중계소는 from_person(발신 사람)을 그대로
//!   대상 기계로 넘기고, 수신측이 그걸 보고 요청 봉투를 씌운다.
//!
//! 배포 위치(넷버드망·클러스터)와 무관하다 — 그건 이 서버를 **어디서 실행하느냐**
//! 일 뿐 코드가 갈리지 않는다. 인증은 사내 공용 릴레이 토큰(`X-Relay-Token`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// 한 기계가 등록한 것. 세션 목록은 그 기계가 주기적으로 갱신한다(peermirror 폴러가
/// /peer-registry 를 올리듯). last_seen 으로 죽은 기계를 걷는다.
#[derive(Clone, Debug)]
struct MachineReg {
    account: String,
    base: String,
    token: Option<String>,
    sessions: Vec<SessionEntry>,
    last_seen: Instant,
}

/// 세션 하나 — 보드에 실리는 최소 정보. **대화·비용은 없다**(거노 결정: 방·제목·상태만).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionEntry {
    pub sid: String,
    pub name: String,
    #[serde(default)]
    pub status: String,
}

/// 죽은 기계로 판정하는 시간 — 이 시간 넘게 재등록이 없으면 목록·라우팅에서 뺀다.
const STALE_AFTER: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Relay {
    machines: Arc<Mutex<HashMap<String, MachineReg>>>,
    /// 사내 공용 릴레이 토큰. 없으면(None) 인증을 요구하지 않는다(로컬 테스트용).
    token: Option<String>,
}

impl Relay {
    pub fn new(token: Option<String>) -> Self {
        Self { machines: Arc::new(Mutex::new(HashMap::new())), token }
    }

    /// 살아 있는(신선한) 기계만. 죽은 것은 잠금 안에서 지운다.
    fn live(&self) -> Vec<(String, MachineReg)> {
        let mut g = self.machines.lock().unwrap();
        let now = Instant::now();
        g.retain(|_, m| now.duration_since(m.last_seen) < STALE_AFTER);
        g.iter().map(|(id, m)| (id.clone(), m.clone())).collect()
    }
}

/// 릴레이 라우터. `serve` 가 이걸 감싸 바인드한다.
pub fn router(relay: Relay) -> Router {
    Router::new()
        .route("/relay/register", post(register))
        .route("/relay/sessions", get(sessions))
        .route("/relay/send", post(send))
        .route("/relay/health", get(|| async { Json(json!({ "ok": true })) }))
        .with_state(relay)
}

fn authed(relay: &Relay, headers: &HeaderMap) -> bool {
    let Some(want) = relay.token.as_deref() else {
        return true; // 토큰 미설정 = 로컬 테스트 모드
    };
    headers
        .get("x-relay-token")
        .and_then(|v| v.to_str().ok())
        .map(|got| got == want)
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct RegisterBody {
    machine_id: String,
    account: String,
    base: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    sessions: Vec<SessionEntry>,
}

/// `POST /relay/register` — 기계가 자기 세션 목록을 올린다(upsert). 기계의 peermirror
/// 폴러가 주기적으로 부른다. base·token 은 라우팅 때 그 기계로 다시 POST 하려고 쥔다.
async fn register(
    State(relay): State<Relay>,
    headers: HeaderMap,
    Json(b): Json<RegisterBody>,
) -> impl IntoResponse {
    if !authed(&relay, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error": "토큰 불일치" })));
    }
    if b.machine_id.is_empty() || b.account.is_empty() || b.base.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "machine_id·account·base 는 필수" })),
        );
    }
    let mut g = relay.machines.lock().unwrap();
    g.insert(
        b.machine_id.clone(),
        MachineReg {
            account: b.account,
            base: b.base.trim_end_matches('/').to_string(),
            token: b.token.filter(|s| !s.is_empty()),
            sessions: b.sessions,
            last_seen: Instant::now(),
        },
    );
    (StatusCode::OK, Json(json!({ "ok": true, "machine_id": b.machine_id })))
}

#[derive(Deserialize)]
struct SessionsQuery {
    /// 특정 계정만 보려면. 비면 사내 전체(공유 보드).
    #[serde(default)]
    account: Option<String>,
}

/// `GET /relay/sessions?account=` — 등록된 기계들의 세션을 모아 준다. **필터**: 각 행은
/// sid·name·machine·account·status 뿐이다(거노 결정: 대화·비용 제외). account 를 주면
/// 그 계정만.
async fn sessions(
    State(relay): State<Relay>,
    headers: HeaderMap,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    if !authed(&relay, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error": "토큰 불일치" })));
    }
    let rows = collect_sessions(&relay.live(), q.account.as_deref());
    (StatusCode::OK, Json(json!({ "ok": true, "sessions": rows })))
}

/// 등록된 기계 목록에서 보드 행을 짓는다 — 순수 함수라 테스트한다. **여기가 필터**:
/// SessionEntry(sid·name·status)에 machine·account 만 얹고 그 밖은 없다.
fn collect_sessions(
    machines: &[(String, MachineReg)],
    account: Option<&str>,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for (id, m) in machines {
        if let Some(a) = account {
            if m.account != a {
                continue;
            }
        }
        for s in &m.sessions {
            out.push(json!({
                "sid": s.sid,
                "name": s.name,
                "status": s.status,
                "machine": id,
                "account": m.account,
            }));
        }
    }
    out
}

#[derive(Deserialize)]
struct SendQuery {
    to_sid: String,
    #[serde(default)]
    from_name: Option<String>,
    /// 발신 사람 — 차 있으면 수신측이 「부탁」 봉투를 씌운다(다른 계정 발신 표식).
    #[serde(default)]
    from_person: Option<String>,
    #[serde(default)]
    from_machine: Option<String>,
}

/// `POST /relay/send?to_sid=&from_name=&from_person=&from_machine=` body=본문 — 대상
/// 세션을 가진 기계를 찾아 그 기계의 `/term/message` 로 넘긴다. 봉투는 수신측이
/// from_person 을 보고 씌우므로 여기선 신원을 **그대로 전달**만 한다.
async fn send(
    State(relay): State<Relay>,
    headers: HeaderMap,
    Query(q): Query<SendQuery>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if !authed(&relay, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error": "토큰 불일치" })));
    }
    let body_text = String::from_utf8_lossy(&body).to_string();
    if body_text.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "본문이 비었어요" })));
    }
    // 대상 sid 를 가진 신선한 기계를 찾는다.
    let Some((_, m)) = relay
        .live()
        .into_iter()
        .find(|(_, m)| m.sessions.iter().any(|s| s.sid == q.to_sid))
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "ok": false, "error": format!("세션 {} 을 가진 기계가 없어요", q.to_sid) })),
        );
    };
    // 그 기계로 라우팅 — send_peer_message 재사용(현-스레드 런타임 블로킹이라 spawn_blocking).
    let base = m.base.clone();
    let token = m.token.clone();
    let (to_sid, from_name, from_person, from_machine) = (
        q.to_sid.clone(),
        q.from_name.unwrap_or_default(),
        q.from_person.unwrap_or_default(),
        q.from_machine.unwrap_or_default(),
    );
    let res = tokio::task::spawn_blocking(move || {
        crate::remote::send_peer_message(
            &base,
            &to_sid,
            &from_name,
            &from_person,
            &from_machine,
            &body_text,
            token.as_deref(),
        )
    })
    .await;
    match res {
        Ok(Ok(())) => (StatusCode::OK, Json(json!({ "ok": true, "delivered_to": q.to_sid }))),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "ok": false, "error": format!("대상 기계 전달 실패: {e}") })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "error": format!("라우팅 태스크 실패: {e}") })),
        ),
    }
}

/// 릴레이를 바인드해 실행한다(블로킹). bin 이 부른다.
pub async fn serve(port: u16, token: Option<String>) -> anyhow::Result<()> {
    let app = router(Relay::new(token));
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let addr = listener.local_addr()?;
    println!("[kasa-relay] listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(account: &str, base: &str, sessions: &[(&str, &str, &str)]) -> MachineReg {
        MachineReg {
            account: account.into(),
            base: base.into(),
            token: None,
            sessions: sessions
                .iter()
                .map(|(sid, name, st)| SessionEntry {
                    sid: sid.to_string(),
                    name: name.to_string(),
                    status: st.to_string(),
                })
                .collect(),
            last_seen: Instant::now(),
        }
    }

    #[test]
    fn 보드는_방제목상태만_싣고_대화비용은_없다() {
        let ms = vec![("맥미니".into(), reg("acme", "http://x", &[("s1", "네네", "idle")]))];
        let rows = collect_sessions(&ms, None);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r["sid"], "s1");
        assert_eq!(r["name"], "네네");
        assert_eq!(r["status"], "idle");
        assert_eq!(r["machine"], "맥미니");
        assert_eq!(r["account"], "acme");
        // 필터: 대화·비용·토큰 같은 건 절대 안 실린다.
        assert!(r.get("cost_usd").is_none());
        assert!(r.get("last_reply").is_none());
        assert!(r.get("token").is_none());
    }

    #[test]
    fn account_필터가_다른_계정을_가린다() {
        let ms = vec![
            ("m1".into(), reg("acme", "http://a", &[("s1", "a", "idle")])),
            ("m2".into(), reg("other", "http://b", &[("s2", "b", "idle")])),
        ];
        let all = collect_sessions(&ms, None);
        assert_eq!(all.len(), 2);
        let acme = collect_sessions(&ms, Some("acme"));
        assert_eq!(acme.len(), 1);
        assert_eq!(acme[0]["sid"], "s1");
    }
}
