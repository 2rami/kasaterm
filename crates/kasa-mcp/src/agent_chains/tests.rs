use super::*;
use axum::extract::State;
use axum::routing::{get, post};
use std::sync::Mutex;

/// What the fake Anthropic endpoints saw: (endpoint, request body or bearer).
pub(crate) type Seen = Arc<Mutex<Vec<(String, String)>>>;

pub(crate) fn seen_calls(calls: &Seen, what: &str) -> Vec<String> {
    calls.lock().unwrap().iter().filter(|(p, _)| p == what).map(|(_, b)| b.clone()).collect()
}

/// Profile: `access-personal*` is a personal organization, `access-team*` a team; anything else 401.
/// Token: `refresh-dead` is refused with invalid_grant, `refresh-busy` gets 429, others rotate.
pub(crate) async fn anthropic() -> (String, Seen) {
    let calls: Seen = Arc::default();
    let app = axum::Router::new()
        .route(
            "/profile",
            get(|State(calls): State<Seen>, headers: axum::http::HeaderMap| async move {
                let bearer = headers["authorization"].to_str().unwrap().trim_start_matches("Bearer ").to_string();
                calls.lock().unwrap().push(("profile".into(), bearer.clone()));
                if bearer.starts_with("access-personal") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"Fixture@Example.invalid"},
                        "organization":{"name":"fixture@example.invalid's Organization","organization_type":"claude_max"}})));
                }
                if bearer.starts_with("access-team") {
                    return (axum::http::StatusCode::OK, axum::Json(json!({"account":{"email":"fixture@example.invalid"},
                        "organization":{"name":"Fixture Team","organization_type":"claude_team"}})));
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

pub(crate) fn endpoints(base: &str) -> Endpoints {
    Endpoints { token: format!("{base}/token"), profile: format!("{base}/profile") }
}

pub(crate) fn state_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kasa-chains-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn service() -> (Arc<Service>, Seen, PathBuf) {
    let dir = state_dir();
    let mut service = Service::open(Some(&dir.join("state.json"))).unwrap();
    let (base, calls) = anthropic().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&base);
    (service, calls, dir)
}

pub(crate) fn offer(access: &str, refresh: &str, left_ms: u64) -> Offer {
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

const HOUR: u64 = 60 * 60 * 1000;

#[tokio::test]
async fn sealing_keys_by_verified_identity_and_keeps_the_refresh_token_on_the_gateway() {
    let (service, calls, dir) = service().await;
    let sealed = service.seal("one", "dev_a", offer("access-personal-1", "refresh-1", 7 * HOUR)).await.unwrap();
    assert_eq!(sealed["key"], "claude:fixture@example.invalid");
    assert_eq!(sealed["held"], false);
    assert_eq!(sealed["access"]["accessToken"], "access-personal-1");
    assert_eq!(seen_calls(&calls, "profile"), vec!["access-personal-1"]);

    let team = service.seal("one", "dev_a", offer("access-team-1", "refresh-t1", 7 * HOUR)).await.unwrap();
    assert_eq!(team["key"], "claude:fixture team");

    let listed = service.list("one").await.unwrap().to_string();
    assert!(listed.contains("access-personal-1") && listed.contains("access-team-1"));
    assert!(!listed.contains("refresh-1") && !listed.contains("refresh-t1"), "a device received a refresh token");
    assert_eq!(service.list("two").await.unwrap()["chains"], json!([]));

    let disk = std::fs::read_to_string(dir.join("agent-chains/one.sealed")).unwrap();
    assert!(!disk.contains("refresh-1") && !disk.contains("access-personal-1"), "the chain is stored in clear");
    let audit = std::fs::read_to_string(dir.join("agent-chains/audit.jsonl")).unwrap();
    assert!(!audit.contains("refresh-") && !audit.contains("access-"), "a token reached the audit log");
}

#[tokio::test]
async fn a_second_device_adopts_the_held_chain_instead_of_replacing_it() {
    let (service, _, _) = service().await;
    service.seal("one", "dev_a", offer("access-personal-1", "refresh-1", 7 * HOUR)).await.unwrap();
    let second = service.seal("one", "dev_b", offer("access-personal-b", "refresh-b", 7 * HOUR)).await.unwrap();
    assert_eq!(second["held"], true);
    assert_eq!(second["access"]["accessToken"], "access-personal-1");
    let book = service.load("one").unwrap();
    assert_eq!(book["claude:fixture@example.invalid"].refresh_token, "refresh-1");
}

#[tokio::test]
async fn offers_that_cannot_be_verified_or_are_about_to_expire_are_refused() {
    let (service, _, _) = service().await;
    assert_eq!(service.seal("one", "d", offer("access-unknown", "refresh-1", 7 * HOUR)).await.unwrap_err(), Error::Rejected);
    assert_eq!(service.seal("one", "d", offer("access-personal-1", "refresh-1", 5 * 60 * 1000)).await.unwrap_err(), Error::Invalid);
    assert_eq!(service.seal("one", "d", offer("access-personal-1", "", 7 * HOUR)).await.unwrap_err(), Error::Invalid);
    let mut setup_token = offer("access-personal-1", "refresh-1", 7 * HOUR);
    setup_token.scopes = vec!["user:profile".into()];
    assert_eq!(service.seal("one", "d", setup_token).await.unwrap_err(), Error::Invalid);
    assert!(service.load("one").unwrap().is_empty());
    assert_eq!(Error::Rejected.status(), 422, "401 would sign the device out of the gateway");
}

#[tokio::test]
async fn the_gateway_refreshes_ahead_of_expiry_and_marks_refused_chains() {
    let (service, calls, _) = service().await;
    service.seal("one", "d", offer("access-personal-1", "refresh-1", 2 * HOUR)).await.unwrap();
    service.seal("one", "d", offer("access-team-1", "refresh-dead", 2 * HOUR)).await.unwrap();
    // Fresh seals settle first: the sealing device's claude may still hold the refresh token for a moment.
    assert_eq!(service.refresh_due().await.0, 0);
    assert!(seen_calls(&calls, "token").is_empty());

    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| c.sealed_at = 0);
    service.save("one", &book).unwrap();
    let (refreshed, notices) = service.refresh_due().await;
    assert_eq!(refreshed, 1);
    assert_eq!(notices.len(), 1, "a refused chain must reach the account's phones");
    assert_eq!((notices[0].account.as_str(), notices[0].key.as_str()), ("one", "claude:fixture team"));
    assert!(notices[0].body.contains("Fixture Team") && !notices[0].body.contains("refresh"));
    let bodies = seen_calls(&calls, "token");
    assert_eq!(bodies.len(), 2);
    let sent: Value = serde_json::from_str(&bodies[0]).unwrap();
    assert_eq!(sent["grant_type"], "refresh_token");
    assert_eq!(sent["client_id"], CLIENT_ID);
    assert!(sent["scope"].as_str().unwrap().contains("user:inference"));

    let book = service.load("one").unwrap();
    let personal = &book["claude:fixture@example.invalid"];
    assert_eq!((personal.access_token.as_str(), personal.refresh_token.as_str()), ("access-personal-2", "refresh-2"));
    assert!(personal.expires_at > now_ms() + 7 * HOUR);
    let team = &book["claude:fixture team"];
    assert!(team.broken && team.refresh_token.is_empty() && team.access_token.is_empty());

    let listed = service.list("one").await.unwrap();
    let states: Vec<(&str, bool)> = listed["chains"].as_array().unwrap().iter()
        .map(|c| (c["state"].as_str().unwrap(), c["access"].is_null())).collect();
    assert_eq!(states, vec![("reconnect_required", true), ("ok", false)]);

    // Not due again for hours; a broken chain is never retried.
    assert_eq!(service.refresh_due().await.0, 0);
    assert_eq!(seen_calls(&calls, "token").len(), 2);

    // Signing in again anywhere replaces the broken chain.
    let again = service.seal("one", "d", offer("access-team-2", "refresh-t2", 7 * HOUR)).await.unwrap();
    assert_eq!(again["held"], false);
    assert!(!service.load("one").unwrap()["claude:fixture team"].broken);
}

#[tokio::test]
async fn a_throttled_refresh_backs_off_and_keeps_the_chain() {
    let (service, calls, _) = service().await;
    service.seal("one", "d", offer("access-personal-1", "refresh-busy", 2 * HOUR)).await.unwrap();
    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| c.sealed_at = 0);
    service.save("one", &book).unwrap();
    assert_eq!(service.refresh_due().await.0, 0);
    assert_eq!(service.refresh_due().await.0, 0);
    assert_eq!(seen_calls(&calls, "token").len(), 1, "a throttled chain was retried at once");
    let chain = &service.load("one").unwrap()["claude:fixture@example.invalid"];
    assert!(!chain.broken && chain.refresh_token == "refresh-busy" && chain.retry_at > now_ms());
}

fn slot_doc(access: &str, refresh: Option<&str>, left_ms: i64) -> Value {
    let mut o = json!({"accessToken":access,"expiresAt":(now_ms() as i64 + left_ms),"scopes":["user:inference","user:profile"],
        "subscriptionType":"max","rateLimitTier":"default_claude_max_20x"});
    if let Some(refresh) = refresh {
        o["refreshToken"] = json!(refresh);
        o["refreshTokenExpiresAt"] = json!(now_ms() + 86_400_000);
    }
    json!({"claudeAiOauth":o,"mcpOAuth":{"server|abc":{"accessToken":"mcp-token"}}})
}

fn access(token: &str, left_ms: u64) -> Access {
    Access { access_token: token.into(), expires_at: now_ms() + left_ms, scopes: vec!["user:inference".into()],
        subscription_type: Some("team".into()), rate_limit_tier: None }
}

#[test]
fn only_live_chains_with_room_before_expiry_are_offered() {
    let now = now_ms();
    let offered = offer_of(&slot_doc("a", Some("r"), 7 * HOUR as i64), now).unwrap();
    assert_eq!((offered.access_token.as_str(), offered.refresh_token.as_str()), ("a", "r"));
    assert_eq!(offered.scopes, vec!["user:inference", "user:profile"]);
    // claude refreshes inside five minutes of expiry; offering near it would race with that refresh.
    assert!(offer_of(&slot_doc("a", Some("r"), 20 * 60 * 1000), now).is_none());
    assert!(offer_of(&slot_doc("a", None, 7 * HOUR as i64), now).is_none());
    assert!(offer_of(&slot_doc("a", Some(""), 7 * HOUR as i64), now).is_none());
    assert!(offer_of(&slot_doc("", Some("r"), 7 * HOUR as i64), now).is_none());
}

#[test]
fn a_slot_takes_the_gateway_token_only_when_it_holds_no_chain_of_its_own() {
    let now = now_ms();
    let fresh = access("g2", 7 * HOUR);
    assert!(!wants_fill(Some(&slot_doc("mine", Some("r"), HOUR as i64)), &fresh, now), "overwrote a slot with its own chain");
    assert!(wants_fill(Some(&slot_doc("g1", None, HOUR as i64)), &fresh, now));
    assert!(!wants_fill(Some(&slot_doc("g2", None, 7 * HOUR as i64)), &fresh, now), "rewrote the same token");
    assert!(!wants_fill(Some(&slot_doc("g3", None, 8 * HOUR as i64)), &fresh, now), "replaced a newer token");
    // claude marks a refused chain with empty tokens; that slot is logged out and may be filled.
    let husk = json!({"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":["user:inference"]}});
    assert!(wants_fill(Some(&husk), &fresh, now));
    assert!(wants_fill(None, &fresh, now));
    let expired = Access { expires_at: now - 1, ..access("old", 0) };
    assert!(!wants_fill(None, &expired, now), "filled an expired token");
}

#[test]
fn the_written_slot_has_no_refresh_field_and_keeps_its_other_entries() {
    let written = with_access(Some(&slot_doc("old", Some("r"), HOUR as i64)), &access("g2", 7 * HOUR));
    let o = written["claudeAiOauth"].as_object().unwrap();
    assert!(!o.contains_key("refreshToken") && !o.contains_key("refreshTokenExpiresAt"),
        "an empty refresh field reads as a dead chain to claude");
    assert_eq!(o["accessToken"], "g2");
    assert_eq!(o["subscriptionType"], "team");
    assert_eq!(o["rateLimitTier"], "default_claude_max_20x", "a missing tier must not erase the stored one");
    assert_eq!(written["mcpOAuth"]["server|abc"]["accessToken"], "mcp-token");
    let husk = json!({"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0}});
    assert!(!with_access(Some(&husk), &access("g2", HOUR))["claudeAiOauth"].as_object().unwrap().contains_key("refreshToken"));
    assert_eq!(with_access(None, &access("g2", HOUR))["claudeAiOauth"]["accessToken"], "g2");
}

/// Makes every chain due: sealed long ago and expiring within the refresh window.
pub(crate) fn age_chains(service: &Service, account: &str) {
    let mut book = service.load(account).unwrap();
    book.values_mut().for_each(|c| {
        c.sealed_at = 0;
        c.expires_at = now_ms() + HOUR;
    });
    service.save(account, &book).unwrap();
}

pub(crate) fn chain_refresh(service: &Service, account: &str, key: &str) -> String {
    service.load(account).unwrap()[key].refresh_token.clone()
}

#[tokio::test]
async fn a_refresh_stuck_close_to_expiry_alerts_once() {
    let (service, _, _) = service().await;
    service.seal("one", "d", offer("access-personal-1", "refresh-busy", 2 * HOUR)).await.unwrap();
    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| {
        c.sealed_at = 0;
        c.expires_at = now_ms() + 40 * 60 * 1000;
    });
    service.save("one", &book).unwrap();
    let (_, notices) = service.refresh_due().await;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].title.contains("막혔어요"));
    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| c.retry_at = 0);
    service.save("one", &book).unwrap();
    assert!(service.refresh_due().await.1.is_empty(), "the same stall alerted twice");
}

struct Docs(std::sync::Mutex<HashMap<String, Value>>);

impl Stores for Docs {
    fn read(&self, slot: &Slot) -> Read {
        self.0.lock().unwrap().get(&slot.id).cloned().map_or(Read::Absent, Read::Present)
    }
    fn write(&self, slot: &Slot, doc: &Value) -> bool {
        self.0.lock().unwrap().insert(slot.id.clone(), doc.clone());
        true
    }
    fn same(&self, _: &Slot) -> bool {
        true
    }
}

#[test]
fn a_device_warns_when_gateway_tokens_run_out_even_with_the_gateway_down() {
    let now = now_ms();
    let docs = Docs(std::sync::Mutex::new(HashMap::from([
        ("a".to_string(), slot_doc("g", None, 20 * 60 * 1000)),
        ("b".to_string(), slot_doc("g", None, 20 * 60 * 1000)),
        ("own".to_string(), slot_doc("mine", Some("r"), 10 * 60 * 1000)),
        ("fresh".to_string(), slot_doc("g2", None, 5 * HOUR as i64)),
        ("team".to_string(), slot_doc("t", None, 5 * HOUR as i64)),
    ])));
    let slots: Vec<Slot> = ["a", "b", "own", "fresh", "team", "unmapped"].iter()
        .map(|id| Slot { id: id.to_string(), store: None, table_key: None }).collect();
    let map: BTreeMap<String, String> = [("a", "claude:x@y"), ("b", "claude:x@y"), ("own", "claude:x@y"),
        ("fresh", "claude:z@y"), ("team", "claude:team")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let down = device_alerts(&slots, &map, &docs, None, now);
    assert_eq!(down.keys().collect::<Vec<_>>(), vec!["stalled|claude:x@y"], "one alert per account, none for healthy or own chains");
    assert!(down["stalled|claude:x@y"].body.contains("관문에 닿지 않아"));
    let chains = vec![Held { key: "claude:team".into(), state: "reconnect_required".into(), ..Default::default() }];
    let up = device_alerts(&slots, &map, &docs, Some(&chains), now);
    assert!(up.contains_key("broken|claude:team") && up["stalled|claude:x@y"].body.contains("갱신하지 못해"));
}
