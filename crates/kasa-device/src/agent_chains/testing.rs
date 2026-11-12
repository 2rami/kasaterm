use super::*;
use axum::extract::State;
use axum::routing::{get, post};
use std::sync::Mutex;

/// What the fake Anthropic endpoints saw: (endpoint, request body or bearer).
pub type Seen = Arc<Mutex<Vec<(String, String)>>>;

pub fn seen_calls(calls: &Seen, what: &str) -> Vec<String> {
    calls.lock().unwrap().iter().filter(|(p, _)| p == what).map(|(_, b)| b.clone()).collect()
}

pub const PERSON: &str = "00000000-0000-4000-8000-000000000001";

pub const PERSONAL_ORG: &str = "00000000-0000-4000-8000-000000000011";

pub const TEAM_ORG: &str = "00000000-0000-4000-8000-000000000022";

pub const GMAIL: &str = "00000000-0000-4000-8000-000000000004";

pub const GMAIL_ORG: &str = "00000000-0000-4000-8000-000000000044";

pub const TWIN: &str = "00000000-0000-4000-8000-000000000003";

pub const TWIN_ORG: &str = "00000000-0000-4000-8000-000000000033";

/// Profile: `access-personal*` is a personal organization, `access-team*` a team, `access-twin*` another
/// account with the same names as the personal one; anything else 401.
/// Token: `refresh-dead` is refused with invalid_grant, `refresh-busy` gets 429, others rotate.
pub async fn anthropic() -> (String, Seen) {
    let calls: Seen = Arc::default();
    let app = axum::Router::new()
        .route(
            "/profile",
            get(|State(calls): State<Seen>, headers: axum::http::HeaderMap| async move {
                let bearer = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_string();
                calls.lock().unwrap().push(("profile".into(), bearer.clone()));
                if bearer.starts_with("access-personal") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"Fixture@Example.invalid","uuid":PERSON},
                        "organization":{"name":"fixture@example.invalid's Organization","organization_type":"claude_max","uuid":PERSONAL_ORG}})));
                }
                if bearer.starts_with("access-team") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"fixture@example.invalid","uuid":PERSON},
                        "organization":{"name":"Fixture Team","organization_type":"claude_team","uuid":TEAM_ORG}})));
                }
                if bearer.starts_with("access-gmail") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"other@example.invalid","uuid":GMAIL},
                        "organization":{"name":"other@example.invalid's Organization","organization_type":"claude_max","uuid":GMAIL_ORG}})));
                }
                // Same email and organization name as `access-personal`, a different provider account.
                if bearer.starts_with("access-twin") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"fixture@example.invalid","uuid":TWIN},
                        "organization":{"name":"fixture@example.invalid's Organization","organization_type":"claude_max","uuid":TWIN_ORG}})));
                }
                (axum::http::StatusCode::UNAUTHORIZED, axum::Json(json!({"error":"invalid_token"})))
            }),
        )
        .route(
            "/token",
            post(|State(calls): State<Seen>, body: String| async move {
                calls.lock().unwrap().push(("token".into(), body.clone()));
                let v: Value = serde_json::from_str(&body).unwrap();
                match v["refresh_token"].as_str().unwrap() {
                    "refresh-dead" => (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error":"invalid_grant"}))),
                    "refresh-busy" => (axum::http::StatusCode::TOO_MANY_REQUESTS, axum::Json(json!({"error":"rate_limited"}))),
                    _ => (axum::http::StatusCode::OK, axum::Json(json!({"access_token":"access-personal-2",
                        "refresh_token":"refresh-2","expires_in":28800,"scope":"user:inference user:profile"}))),
                }
            }),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, calls)
}

pub fn endpoints(base: &str) -> Endpoints {
    Endpoints { token: format!("{base}/token"), profile: format!("{base}/profile") }
}

pub fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kasa-chains-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub async fn service() -> (Arc<Service>, Seen, PathBuf) {
    let dir = state_dir();
    let mut service = Service::open(Some(&dir.join("state.json"))).unwrap();
    let (base, calls) = anthropic().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&base);
    (service, calls, dir)
}

pub fn offer(access: &str, refresh: &str, left_ms: u64) -> Offer {
    Offer {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: now_ms() + left_ms,
        refresh_token_expires_at: now_ms() + 30 * 86_400_000,
        scopes: vec!["user:inference".into(), "user:profile".into(), "user:mcp_servers".into()],
        subscription_type: Some("max".into()),
        rate_limit_tier: Some("default_claude_max_20x".into()),
    }
}

pub const HOUR: u64 = 60 * 60 * 1000;

pub fn slot_doc(access: &str, refresh: Option<&str>, left_ms: i64) -> Value {
    let mut o = json!({"accessToken":access,"expiresAt":(now_ms() as i64 + left_ms),"scopes":["user:inference","user:profile"],
        "subscriptionType":"max","rateLimitTier":"default_claude_max_20x"});
    if let Some(refresh) = refresh {
        o["refreshToken"] = json!(refresh);
        o["refreshTokenExpiresAt"] = json!(now_ms() + 86_400_000);
    }
    json!({"claudeAiOauth":o,"mcpOAuth":{"server|abc":{"accessToken":"mcp-token"}}})
}

pub fn access(token: &str, left_ms: u64) -> Access {
    Access { access_token: token.into(), expires_at: now_ms() + left_ms, scopes: vec!["user:inference".into()],
        subscription_type: Some("team".into()), rate_limit_tier: None }
}

/// Makes every chain due: sealed long ago and expiring within the refresh window.
pub fn age_chains(service: &Service, account: &str) {
    let mut book = service.load(account).unwrap();
    book.values_mut().for_each(|c| {
        // 맡긴 직후 쉬는 틈만 넘긴다 — 0 으로 두면 로그인이 수십 년 전 것처럼 읽혀 새 로그인이 늘 갈아 끼운다.
        c.sealed_at = now_ms().saturating_sub(SEAL_SETTLE_MS + 1);
        c.expires_at = now_ms() + HOUR;
    });
    service.save(account, &book).unwrap();
}

pub fn chain_refresh(service: &Service, account: &str, key: &str) -> String {
    service.load(account).unwrap()[key].refresh_token.clone()
}

pub struct Docs(pub std::sync::Mutex<HashMap<String, Value>>);

impl Stores for Docs {
    fn read(&self, slot: &Slot) -> Read {
        self.0.lock().unwrap().get(&slot.id).cloned().map_or(Read::Absent, Read::Present)
    }
    fn same(&self, _: &Slot) -> bool {
        true
    }
}
