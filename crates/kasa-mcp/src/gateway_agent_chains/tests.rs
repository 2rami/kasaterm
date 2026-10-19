use super::*;
use crate::agent_chains::tests::{anthropic, endpoints, offer, state_dir};
use crate::agent_chains::{Read, Slot, Stores, sync_once};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use futures_util::{SinkExt, StreamExt};

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

    fn compare_and_write(&self, slot: &Slot, expected: Option<&Value>, doc: &Value) -> bool {
        let mut values = self.0.lock().unwrap();
        if values.get(&slot.id) != expected { return false; }
        values.insert(slot.id.clone(), doc.clone());
        true
    }

    fn same(&self, _: &Slot) -> bool {
        true
    }
}

/// A device's view of the gateway that proves token owners against the same fake provider.
fn device_gateway<'a>(gate: &Gate, base: &str, token: &'a str) -> crate::agent_chains::Gateway<'a> {
    let profile = gate.agent_chains.as_ref().unwrap().endpoints.profile.clone();
    crate::agent_chains::Gateway::new(base, token).with_profile(&profile)
}

fn slot(id: &str, table_key: Option<&str>) -> Slot {
    Slot { id: id.into(), store: None, table_key: table_key.map(str::to_string), bench: false, stamped: None }
}

fn logged_in(access: &str, refresh: &str) -> Value {
    let o = offer(access, refresh, 7 * 3_600_000);
    json!({"claudeAiOauth":{"accessToken":o.access_token,"refreshToken":o.refresh_token,"expiresAt":o.expires_at,
        "refreshTokenExpiresAt":o.refresh_token_expires_at,"scopes":o.scopes,"subscriptionType":"max",
        "rateLimitTier":"default_claude_max_20x"},"mcpOAuth":{"server|1":{"accessToken":"mcp"}}})
}

const PERSONAL: &str = "claude:fixture@example.invalid";

fn owner_who() -> crate::agent_chains::Who {
    use crate::agent_chains::tests::{PERSON, PERSONAL_ORG};
    crate::agent_chains::Who::new(PERSON, PERSONAL_ORG).unwrap()
}

fn owned(token: &str, who: crate::agent_chains::Who) -> crate::agent_chains::Owner {
    crate::agent_chains::Owner { key: PERSONAL.into(), fingerprint: crate::agent_chains::fingerprint(token), who }
}

fn access_only(access: &str, left_ms: u64) -> Value {
    let mut doc = logged_in(access, "unused");
    let o = doc["claudeAiOauth"].as_object_mut().unwrap();
    o.remove("refreshToken");
    o.remove("refreshTokenExpiresAt");
    o.insert("expiresAt".into(), json!(offer("x", "x", left_ms).expires_at));
    doc
}
const TEAM: &str = "claude:fixture team";

struct LoginBeforeCommit {
    memory: Memory,
    replacement: std::sync::Mutex<Option<Value>>,
    commits: std::sync::atomic::AtomicUsize,
}

impl Stores for LoginBeforeCommit {
    fn read(&self, slot: &Slot) -> Read { self.memory.read(slot) }
    fn same(&self, _: &Slot) -> bool { true }
    fn compare_and_write(&self, slot: &Slot, expected: Option<&Value>, next: &Value) -> bool {
        self.commits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut values = self.memory.0.lock().unwrap();
        if let Some(login) = self.replacement.lock().unwrap().take() {
            values.insert(slot.id.clone(), login);
        }
        if values.get(&slot.id) != expected { return false; }
        values.insert(slot.id.clone(), next.clone());
        true
    }
}

#[tokio::test]
async fn a_delayed_pull_or_seal_never_erases_a_completed_phone_login() {
    for sealing in [false, true] {
        let (base, _, token, _, gate) = rig().await;
        let base = base.trim_end_matches("/relay");
        let mut old = logged_in("access-personal-old", "refresh-old");
        if !sealing {
            old["claudeAiOauth"].as_object_mut().unwrap().remove("refreshToken");
            old["claudeAiOauth"]["expiresAt"] = json!(offer("fixture", "fixture", 60_000).expires_at);
            gate.agent_chains.as_ref().unwrap().seal("one", "dev_desktop_one",
                offer("access-personal-gateway", "refresh-gateway", 7 * 3_600_000)).await.unwrap();
        }
        let fresh = logged_in("access-personal-phone", "refresh-phone");
        let store = LoginBeforeCommit {
            memory: Memory::with(&[("acct-1", old)]),
            replacement: std::sync::Mutex::new(Some(fresh.clone())),
            commits: std::sync::atomic::AtomicUsize::new(0),
        };
        let gateway = device_gateway(&gate, base, &token);
        let ledger = crate::agent_chains::recovery::Ledger::default();
        let (learned, _) = sync_once(&gateway, &[slot("acct-1", Some(PERSONAL))],
            &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
        assert_eq!(store.commits.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(store.memory.0.lock().unwrap().get("acct-1"), Some(&fresh));
        assert!(store.memory.holds_refresh());
        assert!(learned.keys.is_empty() && learned.owners.is_empty(), "a refused stale commit must not adopt the old chain");
    }
}

#[tokio::test]
async fn devices_hand_their_chains_to_the_gateway_and_share_one_rotation() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let allow = || true;
    let (mini_token, spare_token) = (format!("{token}b"), format!("{token}c"));
    let (laptop, mini, spare) = (
        device_gateway(&gate, &base, &token),
        device_gateway(&gate, &base, &mini_token),
        device_gateway(&gate, &base, &spare_token),
    );
    let laptop_ledger = crate::agent_chains::recovery::Ledger::default();
    let mini_ledger = crate::agent_chains::recovery::Ledger::default();
    let spare_ledger = crate::agent_chains::recovery::Ledger::default();

    // The laptop signed in once; its slot hands the chain over and keeps only the access token.
    let laptop_slots = [slot("acct-1", Some(PERSONAL))];
    let laptop_store = Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]);
    let (learned, _) = sync_once(&laptop, &laptop_slots, &BTreeMap::new(), &BTreeMap::new(), &laptop_store, &allow, &laptop_ledger).await.unwrap();
    assert_eq!(learned.keys.get("acct-1").map(String::as_str), Some(PERSONAL));
    assert_eq!(learned.owners["acct-1"].fingerprint, crate::agent_chains::fingerprint("access-personal-1"));
    assert_eq!(learned.owners["acct-1"].who, owner_who());
    assert!(!laptop_store.holds_refresh(), "the laptop kept a refresh token after sealing");
    assert_eq!(laptop_store.oauth("acct-1")["accessToken"], "access-personal-1");
    assert_eq!(laptop_store.0.lock().unwrap()["acct-1"]["mcpOAuth"]["server|1"]["accessToken"], "mcp");

    // The mini: a logged-out husk that a person chose for the account (adopt) is filled, a husk named only by the
    // display table is not, its own team login is sealed, and a slot with no stored item and no choice stays untouched.
    let husk = json!({"claudeAiOauth":{"accessToken":"","refreshToken":"","expiresAt":0,"scopes":["user:inference"]}});
    let mini_slots = [slot("acct-6", Some(PERSONAL)), slot("acct-5", None), slot("acct-9", Some(PERSONAL)), slot("acct-8", Some(PERSONAL))];
    let mini_store = Memory::with(&[("acct-6", husk.clone()), ("acct-8", husk.clone()), ("acct-5", logged_in("access-team-1", "refresh-t1"))]);
    let adopted: BTreeMap<String, String> = [("acct-6".to_string(), PERSONAL.to_string())].into();
    let (learned, _) = sync_once(&mini, &mini_slots, &adopted, &BTreeMap::new(), &mini_store, &allow, &mini_ledger).await.unwrap();
    assert_eq!(mini_store.oauth("acct-6")["accessToken"], "access-personal-1");
    assert_eq!(mini_store.oauth("acct-8"), husk["claudeAiOauth"], "a display name alone filled a husk");
    assert_eq!(learned.owners["acct-6"].who, owner_who(), "the filled slot records the provider-proven owner");
    assert_eq!(learned.owners["acct-5"].who.organization_uuid, crate::agent_chains::tests::TEAM_ORG);
    assert_eq!(mini_store.oauth("acct-5")["accessToken"], "access-team-1");
    assert!(mini_store.oauth("acct-9").is_null());
    assert!(["acct-5", "acct-6"].iter().all(|id| mini_store.oauth(id).get("refreshToken").is_none()));
    assert_eq!(learned.keys.get("acct-5").map(String::as_str), Some(TEAM));

    // The gateway rotates once; every device picks the new access token up on its next cycle.
    let service = gate.agent_chains.clone().unwrap();
    let mut book: BTreeMap<String, Value> = BTreeMap::new();
    let listed = service.list("one").await.unwrap();
    for chain in listed["chains"].as_array().unwrap() {
        book.insert(chain["key"].as_str().unwrap().into(), chain.clone());
    }
    assert_eq!(book.len(), 2);
    crate::agent_chains::tests::age_chains(&service, "one");
    assert_eq!(service.refresh_due().await.0, 2);
    let mapped: BTreeMap<String, String> = [("acct-1".to_string(), PERSONAL.to_string())].into();
    sync_once(&laptop, &laptop_slots, &mapped, &BTreeMap::new(), &laptop_store, &allow, &laptop_ledger).await.unwrap();
    sync_once(&mini, &mini_slots, &[("acct-5".to_string(), TEAM.to_string())].into(), &BTreeMap::new(), &mini_store, &allow, &mini_ledger).await.unwrap();
    assert_eq!(laptop_store.oauth("acct-1")["accessToken"], "access-personal-2");
    assert_eq!(mini_store.oauth("acct-6")["accessToken"], "access-personal-2");
    assert!(!laptop_store.holds_refresh() && ["acct-5", "acct-6"].iter().all(|id| mini_store.oauth(id).get("refreshToken").is_none()));

    // A third device that logged in on its own gives up its chain for the one the gateway holds.
    let spare_slots = [slot("acct-2", None)];
    let spare_store = Memory::with(&[("acct-2", logged_in("access-personal-c", "refresh-c"))]);
    sync_once(&spare, &spare_slots, &BTreeMap::new(), &BTreeMap::new(), &spare_store, &allow, &spare_ledger).await.unwrap();
    assert_eq!(spare_store.oauth("acct-2")["accessToken"], "access-personal-2");
    assert!(!spare_store.holds_refresh());
    let chain = crate::agent_chains::tests::chain_refresh(&service, "one", PERSONAL);
    assert_eq!(chain, "refresh-2", "a device's own chain replaced the shared one");
}

#[tokio::test]
async fn a_slot_whose_chain_moved_on_is_not_overwritten_and_an_old_gateway_is_left_alone() {
    let (base, client, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    // claude rotated the slot between the offer and the write: the stale offer must not strip the new chain.
    struct Rotating(Memory, std::sync::atomic::AtomicUsize);
    impl Stores for Rotating {
        fn read(&self, slot: &Slot) -> Read {
            if self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                self.0.0.lock().unwrap().insert(slot.id.clone(), logged_in("access-personal-r", "refresh-rotated"));
            }
            self.0.read(slot)
        }
        fn compare_and_write(&self, slot: &Slot, expected: Option<&Value>, doc: &Value) -> bool {
            self.0.compare_and_write(slot, expected, doc)
        }
        fn same(&self, _: &Slot) -> bool {
            true
        }
    }
    let store = Rotating(Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]), Default::default());
    let slots = [slot("acct-1", None)];
    sync_once(&device, &slots, &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert_eq!(store.0.oauth("acct-1")["refreshToken"], "refresh-rotated");

    // A gateway without the endpoint answers 404: the device stops and keeps every login as it is.
    let old = device_gateway(&gate, &format!("{base}/nowhere"), &token);
    let store = Memory::with(&[("acct-1", logged_in("access-personal-1", "refresh-1"))]);
    let unsupported = sync_once(&old, &slots, &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await;
    assert_eq!(unsupported.err(), Some(crate::agent_chains::SyncError::Unsupported));
    assert_eq!(store.oauth("acct-1")["refreshToken"], "refresh-1");
    drop(client);
}

async fn chain_uplink(base: &str, token: &str, machine: &str, capable: bool) -> tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
> {
    let url = format!("{}/uplink", base.replacen("http://", "ws://", 1));
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        json!({
            "t":"hello", "key":format!("fixture-key-{machine}-0123456789"), "slugs":[],
            "machine":machine, "machine_id":machine, "device_token":token, "proto":2,
            "capabilities":{"agent_chains":capable}
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();
    let answer = ws.next().await.unwrap().unwrap();
    let tokio_tungstenite::tungstenite::Message::Text(answer) = answer else { panic!("missing hello answer") };
    assert_eq!(serde_json::from_str::<Value>(answer.as_str()).unwrap()["t"], "ok");
    ws
}

async fn receives_chain_hint(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(150);
    loop {
        let Ok(Some(Ok(message))) = tokio::time::timeout_at(deadline, ws.next()).await else { return false };
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            let value: Value = serde_json::from_str(text.as_str()).unwrap_or_default();
            if value["t"] == "agent_chains" {
                assert_eq!(value.as_object().unwrap().len(), 1, "the hint carried account or token data");
                return true;
            }
        }
    }
}

#[tokio::test]
async fn saved_chains_hint_only_capable_authenticated_uplinks_in_the_same_account() {
    let (base, client, token, other, gate) = rig().await;
    let mut capable = chain_uplink(&base, &token, "machine-one", true).await;
    let mut legacy = chain_uplink(&base, &format!("{token}b"), "machine-one", false).await;
    let mut foreign = chain_uplink(&base, &other, "machine-two", true).await;

    let sealed = client
        .post(format!("{base}/agent-chains/seal"))
        .bearer_auth(&token)
        .json(&offer("access-personal-1", "refresh-1", 7 * 3_600_000))
        .send()
        .await
        .unwrap();
    assert_eq!(sealed.status(), 200);
    assert!(receives_chain_hint(&mut capable).await);
    assert!(!receives_chain_hint(&mut legacy).await);
    assert!(!receives_chain_hint(&mut foreign).await);

    let service = gate.agent_chains.clone().unwrap();
    crate::agent_chains::tests::age_chains(&service, "one");
    let (_, _, changed) = service.refresh_due().await;
    assert_eq!(changed, vec!["one"]);
    for account in changed {
        gate.hint_agent_chains(&account);
    }
    assert!(receives_chain_hint(&mut capable).await);
}

#[tokio::test]
async fn an_outside_login_on_the_workbench_never_rebinds_the_slot_or_reaches_its_vault() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let service = gate.agent_chains.clone().unwrap();
    service.seal("one", "dev_desktop_one", offer("access-personal-g", "refresh-g", 7 * 3_600_000)).await.unwrap();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let map: BTreeMap<String, String> = [("acct-6".to_string(), PERSONAL.to_string())].into();
    let owners: BTreeMap<_, _> = [("acct-6".to_string(), owned("access-personal-old", owner_who()))].into();
    let bench = Slot { id: "acct-6".into(), store: None, table_key: Some(PERSONAL.into()), bench: true, stamped: None };

    // `/login` put another account (with its own refresh token) on the workbench of the selected slot.
    let outside = logged_in("access-twin-outside", "refresh-outside");
    let store = Memory::with(&[("acct-6", outside.clone())]);
    let (learned, _) = sync_once(&device, std::slice::from_ref(&bench), &map, &owners, &store, &|| true, &ledger).await.unwrap();
    assert!(learned.keys.is_empty() && learned.owners.is_empty(), "the workbench rebound the slot");
    assert_eq!(store.0.lock().unwrap()["acct-6"], outside, "the outside login lost its refresh token");
    assert_eq!(crate::agent_chains::tests::chain_refresh(&service, "one", PERSONAL), "refresh-g");

    // The same names with another provider account never receive the slot's chain, on the workbench or in a vault.
    for (slot, owners) in [(bench, owners.clone()), (slot("acct-6", Some(PERSONAL)), owners.clone()), (slot("acct-6", Some(PERSONAL)), BTreeMap::new())] {
        let twin = access_only("access-twin-gateway", 60_000);
        let store = Memory::with(&[("acct-6", twin.clone())]);
        let (learned, _) = sync_once(&device, std::slice::from_ref(&slot), &map, &owners, &store, &|| true, &ledger).await.unwrap();
        assert_eq!(store.0.lock().unwrap()["acct-6"], twin, "an account with the same email and organization name was overwritten");
        assert!(learned.owners.is_empty());
    }
}

#[tokio::test]
async fn same_names_with_another_provider_account_keep_their_own_login() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let service = gate.agent_chains.clone().unwrap();
    service.seal("one", "dev_desktop_one", offer("access-personal-1", "refresh-1", 7 * 3_600_000)).await.unwrap();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let login = logged_in("access-twin-1", "refresh-twin");
    let store = Memory::with(&[("acct-2", login.clone())]);
    let (learned, _) = sync_once(&device, &[slot("acct-2", None)], &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert!(learned.keys.is_empty() && learned.owners.is_empty());
    assert_eq!(store.0.lock().unwrap()["acct-2"], login, "the device gave up its login for another person's chain");
    assert_eq!(crate::agent_chains::tests::chain_refresh(&service, "one", PERSONAL), "refresh-1", "the gateway replaced the held chain");
}

#[tokio::test]
async fn a_rotation_whose_old_token_is_already_dead_is_proven_by_the_owner_record_only() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let service = gate.agent_chains.clone().unwrap();
    service.seal("one", "dev_desktop_one", offer("access-personal-1", "refresh-1", 7 * 3_600_000)).await.unwrap();
    crate::agent_chains::tests::age_chains(&service, "one");
    assert_eq!(service.refresh_due().await.0, 1);
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let map: BTreeMap<String, String> = [("acct-1".to_string(), PERSONAL.to_string())].into();
    // The provider no longer answers for the old token (revoked by the rotation, profile cache expired).
    let dead = access_only("access-gone-1", 60_000);

    let store = Memory::with(&[("acct-1", dead.clone())]);
    sync_once(&device, &[slot("acct-1", Some(PERSONAL))], &map, &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert_eq!(store.0.lock().unwrap()["acct-1"], dead, "a mapping alone overwrote an unproven login");

    let owners: BTreeMap<_, _> = [("acct-1".to_string(), owned("access-gone-1", owner_who()))].into();
    let (learned, _) = sync_once(&device, &[slot("acct-1", Some(PERSONAL))], &map, &owners, &store, &|| true, &ledger).await.unwrap();
    assert_eq!(store.oauth("acct-1")["accessToken"], "access-personal-2");
    assert_eq!(learned.owners["acct-1"].fingerprint, crate::agent_chains::fingerprint("access-personal-2"));

    // An owner record of someone else stops the same mapping.
    let twin = crate::agent_chains::Who::new(crate::agent_chains::tests::TWIN, crate::agent_chains::tests::TWIN_ORG).unwrap();
    let owners: BTreeMap<_, _> = [("acct-1".to_string(), owned("access-gone-1", twin))].into();
    let store = Memory::with(&[("acct-1", dead.clone())]);
    sync_once(&device, &[slot("acct-1", Some(PERSONAL))], &map, &owners, &store, &|| true, &ledger).await.unwrap();
    assert_eq!(store.0.lock().unwrap()["acct-1"], dead);
}

#[tokio::test]
async fn a_display_name_is_only_a_hint_until_the_current_token_proves_the_same_owner() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let service = gate.agent_chains.clone().unwrap();
    service.seal("one", "dev_desktop_one", offer("access-personal-g", "refresh-g", 7 * 3_600_000)).await.unwrap();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let slots = [slot("acct-3", Some(PERSONAL)), slot("acct-4", Some(PERSONAL))];
    let store = Memory::with(&[("acct-3", access_only("access-personal-stale", 60_000)), ("acct-4", access_only("access-twin-stale", 60_000))]);
    let (learned, _) = sync_once(&device, &slots, &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert_eq!(store.oauth("acct-3")["accessToken"], "access-personal-g");
    assert_eq!(learned.keys.get("acct-3").map(String::as_str), Some(PERSONAL));
    assert_eq!(store.oauth("acct-4")["accessToken"], "access-twin-stale");
    assert!(!learned.keys.contains_key("acct-4"));
}

#[tokio::test]
async fn a_login_selected_before_its_first_handover_is_sealed_when_the_bench_stamp_proves_it() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    // finish_login → the person selects the new slot at once: it is on the workbench with no mapping yet.
    let mut bench = Slot { id: "acct-7".into(), store: None, table_key: Some(PERSONAL.into()), bench: true, stamped: Some(owner_who()) };
    let store = Memory::with(&[("acct-7", logged_in("access-personal-new", "refresh-new"))]);
    let (learned, _) = sync_once(&device, std::slice::from_ref(&bench), &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert_eq!(learned.keys.get("acct-7").map(String::as_str), Some(PERSONAL));
    assert_eq!(learned.owners["acct-7"].who, owner_who());
    assert!(!store.holds_refresh(), "the first handover of a proven login was skipped");

    // Without a stamp naming this slot, or with a stamp of another account, the workbench keeps its login.
    for stamped in [None, crate::agent_chains::Who::new(crate::agent_chains::tests::TWIN, crate::agent_chains::tests::TWIN_ORG)] {
        bench.stamped = stamped;
        let login = logged_in("access-personal-other", "refresh-other");
        let store = Memory::with(&[("acct-7", login.clone())]);
        let (learned, _) = sync_once(&device, std::slice::from_ref(&bench), &BTreeMap::new(), &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
        assert!(learned.keys.is_empty());
        assert_eq!(store.0.lock().unwrap()["acct-7"], login);
    }
}

#[tokio::test]
async fn late_answers_record_nothing_for_a_signed_out_deleted_or_relogged_slot() {
    let (base, _, token, _, gate) = rig().await;
    let base = base.trim_end_matches("/relay").to_string();
    let service = gate.agent_chains.clone().unwrap();
    service.seal("one", "dev_desktop_one", offer("access-personal-g", "refresh-g", 7 * 3_600_000)).await.unwrap();
    let device = device_gateway(&gate, &base, &token);
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let map: BTreeMap<String, String> = [("acct-1".to_string(), PERSONAL.to_string())].into();
    let seen = [slot("acct-1", Some(PERSONAL))];
    let store = Memory::with(&[("acct-1", access_only("access-personal-g", 7 * 3_600_000))]);
    // The device signed out while the list was in flight: the owner of the held token is not learned.
    let (learned, _) = sync_once(&device, &seen, &map, &BTreeMap::new(), &store, &|| false, &ledger).await.unwrap();
    assert!(learned.owners.is_empty());
    let (learned, _) = sync_once(&device, &seen, &map, &BTreeMap::new(), &store, &|| true, &ledger).await.unwrap();
    assert_eq!(learned.owners["acct-1"].fingerprint, crate::agent_chains::fingerprint("access-personal-g"));

    let again = |learned: &crate::agent_chains::Learned| crate::agent_chains::Learned { keys: learned.keys.clone(), owners: learned.owners.clone() };
    let kept = crate::agent_chains::still_current(again(&learned), &seen, &seen, &store);
    assert_eq!(kept.owners.len(), 1);
    // Deleted, moved to another store, or relogged before the save: nothing is written for it.
    assert!(crate::agent_chains::still_current(again(&learned), &seen, &[], &store).owners.is_empty());
    let moved = [Slot { store: Some("elsewhere".into()), ..slot("acct-1", Some(PERSONAL)) }];
    assert!(crate::agent_chains::still_current(again(&learned), &seen, &moved, &store).owners.is_empty());
    let relogged = Memory::with(&[("acct-1", logged_in("access-personal-relogin", "refresh-relogin"))]);
    assert!(crate::agent_chains::still_current(again(&learned), &seen, &seen, &relogged).owners.is_empty());
}
