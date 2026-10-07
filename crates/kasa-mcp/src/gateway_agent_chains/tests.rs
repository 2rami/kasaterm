use super::*;
use crate::agent_chains::tests::{anthropic, endpoints, offer, state_dir};
use crate::agent_chains::{Read, Slot, Stores, sync_once};
use serde_json::{json, Value};
use std::collections::BTreeMap;

async fn rig() -> (String, reqwest::Client, String, String, Gate) {
    let dir = state_dir();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    for name in ["one", "two"] {
        accounts.accounts.insert(name.into(), crate::relay_auth::Account {
            pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture", 1),
            disabled: false,
            created: 1,
            login: None,
        });
    }
    crate::relay_auth::save_accounts(&dir.join("accounts.json"), &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(dir.join("accounts.json")));
    // Reopen with the fake Anthropic endpoints; the gateway's own store holds the single-writer lock.
    gate.agent_chains = None;
    let mut service = crate::agent_chains::Service::open(Some(&dir.join("state.json"))).unwrap();
    let (base, _) = anthropic().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&base);
    gate.agent_chains = Some(service);
    let token = crate::relay_auth::new_token();
    let other = crate::relay_auth::new_token();
    for (id, account, hash) in [
        ("dev_desktop_one", "one", crate::relay_auth::token_hash(&token)),
        ("dev_desktop_two", "two", crate::relay_auth::token_hash(&other)),
        ("dev_mini_one", "one", crate::relay_auth::token_hash(&format!("{token}b"))),
        ("dev_spare_one", "one", crate::relay_auth::token_hash(&format!("{token}c"))),
    ] {
        gate.devices.lock().unwrap().insert(id.into(), DeviceRec {
            token_hash: hash,
            account: account.into(),
            kind: "desktop".into(),
            machine_id: Some(format!("machine-{account}")),
            label: "Laptop".into(),
            created: 1,
            last_seen: 1,
            revoked_at: None,
        });
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let served = gate.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    (format!("http://{address}/relay"), client, token, other, gate)
}

#[tokio::test]
async fn devices_seal_and_receive_access_tokens_but_never_refresh_tokens() {
    let (base, client, token, _, gate) = rig().await;
    let sealed = client.post(format!("{base}/agent-chains/seal")).bearer_auth(&token)
        .json(&offer("access-personal-1", "refresh-1", 7 * 3_600_000)).send().await.unwrap();
    assert_eq!(sealed.status(), 200);
    assert_eq!(sealed.headers()["cache-control"], "no-store");
    let body = sealed.text().await.unwrap();
    assert!(!body.contains("refresh-1"), "the seal answer echoed the refresh token");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!((body["key"].as_str(), body["held"].as_bool()), (Some("claude:fixture@example.invalid"), Some(false)));

    let listed = client.get(format!("{base}/agent-chains")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(listed.status(), 200);
    let listed = listed.text().await.unwrap();
    assert!(listed.contains("access-personal-1") && !listed.contains("refresh-1"));

    // An unverifiable token is the chain's problem, not the device's sign-in.
    let refused = client.post(format!("{base}/agent-chains/seal")).bearer_auth(&token)
        .json(&offer("access-unknown", "refresh-x", 7 * 3_600_000)).send().await.unwrap();
    assert_eq!(refused.status(), 422);

    gate.devices.lock().unwrap().get_mut("dev_desktop_one").unwrap().revoked_at = Some(2);
    let revoked = client.get(format!("{base}/agent-chains")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(revoked.status(), 401);
}

#[tokio::test]
async fn chains_need_a_device_token_and_stay_inside_the_account() {
    let (base, client, token, other_token, _) = rig().await;
    assert_eq!(client.get(format!("{base}/agent-chains")).send().await.unwrap().status(), 401);
    assert_eq!(client.post(format!("{base}/agent-chains/seal")).bearer_auth("kdt_not_a_device")
        .json(&offer("access-personal-1", "refresh-1", 7 * 3_600_000)).send().await.unwrap().status(), 401);
    assert_eq!(client.post(format!("{base}/agent-chains/seal")).bearer_auth(&token)
        .body("{\"account\":\"two\"}").send().await.unwrap().status(), 400);
    client.post(format!("{base}/agent-chains/seal")).bearer_auth(&token)
        .json(&offer("access-personal-1", "refresh-1", 7 * 3_600_000)).send().await.unwrap();
    let other = client.get(format!("{base}/agent-chains")).bearer_auth(&other_token).send().await.unwrap();
    assert_eq!(other.status(), 200);
    let other = other.text().await.unwrap();
    assert!(!other.contains("access-personal-1"), "another account read the chain");
}

/// A device's slots in memory — tests never touch the user's keychain.
#[derive(Default)]
struct Memory(std::sync::Mutex<HashMap<String, Value>>);

impl Memory {
    fn with(docs: &[(&str, Value)]) -> Self {
        Self(std::sync::Mutex::new(docs.iter().map(|(id, doc)| (id.to_string(), doc.clone())).collect()))
    }

    fn oauth(&self, id: &str) -> Value {
        self.0.lock().unwrap().get(id).map_or(Value::Null, |doc| doc["claudeAiOauth"].clone())
    }

    fn holds_refresh(&self) -> bool {
        self.0.lock().unwrap().values().any(|doc| doc["claudeAiOauth"].get("refreshToken").is_some())
    }
}

impl Stores for Memory {
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

fn slot(id: &str, table_key: Option<&str>) -> Slot {
    Slot { id: id.into(), store: None, table_key: table_key.map(str::to_string) }
}

fn logged_in(access: &str, refresh: &str) -> Value {
    let o = offer(access, refresh, 7 * 3_600_000);
    json!({"claudeAiOauth":{"accessToken":o.access_token,"refreshToken":o.refresh_token,"expiresAt":o.expires_at,
        "refreshTokenExpiresAt":o.refresh_token_expires_at,"scopes":o.scopes,"subscriptionType":"max",
        "rateLimitTier":"default_claude_max_20x"},"mcpOAuth":{"server|1":{"accessToken":"mcp"}}})
}

const PERSONAL: &str = "claude:fixture@example.invalid";
const TEAM: &str = "claude:fixture team";

#[tokio::test]
async fn devices_hand_their_chains_to_the_gateway_and_share_one_rotation() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let allow = || true;
    let (mini_token, spare_token) = (format!("{token}b"), format!("{token}c"));
    let (laptop, mini, spare) = (
        crate::agent_chains::Gateway::new(&base, &token),
        crate::agent_chains::Gateway::new(&base, &mini_token),
        crate::agent_chains::Gateway::new(&base, &spare_token),
    );

    // The laptop signed in once; its slot hands the chain over and keeps only the access token.
    let laptop_slots = [slot("acct-1", Some(PERSONAL))];
    let laptop_store = Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]);
    let learned = sync_once(&laptop, &laptop_slots, &BTreeMap::new(), &laptop_store, &allow).await.unwrap();
    assert_eq!(learned.get("acct-1").map(String::as_str), Some(PERSONAL));
    assert!(!laptop_store.holds_refresh(), "the laptop kept a refresh token after sealing");
    assert_eq!(laptop_store.oauth("acct-1")["accessToken"], "access-personal-1");
    assert_eq!(laptop_store.0.lock().unwrap()["acct-1"]["mcpOAuth"]["server|1"]["accessToken"], "mcp");

    // The mini: a logged-out husk of the same account is filled, its own team login is sealed, and a
    // slot with no stored item and no explicit choice stays untouched.
    let husk = json!({"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":["user:inference"]}});
    let mini_slots = [slot("acct-6", Some(PERSONAL)), slot("acct-5", None), slot("acct-9", Some(PERSONAL))];
    let mini_store = Memory::with(&[("acct-6", husk), ("acct-5", logged_in("access-team-1", "refresh-t1"))]);
    let learned = sync_once(&mini, &mini_slots, &BTreeMap::new(), &mini_store, &allow).await.unwrap();
    assert_eq!(mini_store.oauth("acct-6")["accessToken"], "access-personal-1");
    assert_eq!(mini_store.oauth("acct-5")["accessToken"], "access-team-1");
    assert!(mini_store.oauth("acct-9").is_null());
    assert!(!mini_store.holds_refresh());
    assert_eq!(learned.get("acct-5").map(String::as_str), Some(TEAM));

    // The gateway rotates once; every device picks the new access token up on its next cycle.
    let service = gate.agent_chains.clone().unwrap();
    let mut book: BTreeMap<String, Value> = BTreeMap::new();
    let listed = service.list("one").await.unwrap();
    for chain in listed["chains"].as_array().unwrap() {
        book.insert(chain["key"].as_str().unwrap().into(), chain.clone());
    }
    assert_eq!(book.len(), 2);
    crate::agent_chains::tests::age_chains(&service, "one");
    assert_eq!(service.refresh_due().await, 2);
    let mapped: BTreeMap<String, String> = [("acct-1".to_string(), PERSONAL.to_string())].into();
    sync_once(&laptop, &laptop_slots, &mapped, &laptop_store, &allow).await.unwrap();
    sync_once(&mini, &mini_slots, &[("acct-5".to_string(), TEAM.to_string())].into(), &mini_store, &allow).await.unwrap();
    assert_eq!(laptop_store.oauth("acct-1")["accessToken"], "access-personal-2");
    assert_eq!(mini_store.oauth("acct-6")["accessToken"], "access-personal-2");
    assert!(!laptop_store.holds_refresh() && !mini_store.holds_refresh());

    // A third device that logged in on its own gives up its chain for the one the gateway holds.
    let spare_slots = [slot("acct-2", None)];
    let spare_store = Memory::with(&[("acct-2", logged_in("access-personal-c", "refresh-c"))]);
    sync_once(&spare, &spare_slots, &BTreeMap::new(), &spare_store, &allow).await.unwrap();
    assert_eq!(spare_store.oauth("acct-2")["accessToken"], "access-personal-2");
    assert!(!spare_store.holds_refresh());
    let chain = crate::agent_chains::tests::chain_refresh(&service, "one", PERSONAL);
    assert_eq!(chain, "refresh-2", "a device's own chain replaced the shared one");
}

#[tokio::test]
async fn a_slot_whose_chain_moved_on_is_not_overwritten_and_an_old_gateway_is_left_alone() {
    let (base, client, token, _, _) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let device = crate::agent_chains::Gateway::new(&base, &token);
    // claude rotated the slot between the offer and the write: the stale offer must not strip the new chain.
    struct Rotating(Memory, std::sync::atomic::AtomicUsize);
    impl Stores for Rotating {
        fn read(&self, slot: &Slot) -> Read {
            if self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                self.0.write(slot, &logged_in("access-personal-r", "refresh-rotated"));
            }
            self.0.read(slot)
        }
        fn write(&self, slot: &Slot, doc: &Value) -> bool {
            self.0.write(slot, doc)
        }
        fn same(&self, _: &Slot) -> bool {
            true
        }
    }
    let store = Rotating(Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]), Default::default());
    let slots = [slot("acct-1", None)];
    sync_once(&device, &slots, &BTreeMap::new(), &store, &|| true).await.unwrap();
    assert_eq!(store.0.oauth("acct-1")["refreshToken"], "refresh-rotated");

    // A gateway without the endpoint answers 404: the device stops and keeps every login as it is.
    let old = crate::agent_chains::Gateway::new(&format!("{base}/nowhere"), &token);
    let store = Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]);
    let unsupported = sync_once(&old, &slots, &BTreeMap::new(), &store, &|| true).await;
    assert_eq!(unsupported.unwrap_err(), crate::agent_chains::SyncError::Unsupported);
    assert_eq!(store.oauth("acct-1")["refreshToken"], "refresh-1");
    drop(client);
}
