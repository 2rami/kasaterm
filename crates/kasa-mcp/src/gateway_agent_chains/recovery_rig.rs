//! Two devices on one KASA account against the real gateway router: the shared choice through
//! `/relay/account-sync` and the chain handover through `/relay/agent-chains`, with a fake provider
//! and in-memory credential stores — no keychain, no real account.
//!
//! The case it reproduces: the laptop runs the personal account in `acct-5`; the mini's `acct-6` is
//! labelled for that account but its vault holds the Gmail login (a duplicate of `acct-1`). The mini
//! recovers by following the laptop's verified choice into a newly adopted slot — neither Gmail slot
//! is overwritten, and the old build's slot is never edited by hand.

use super::*;
use crate::account_sync::{choice, local, PendingApply};
use crate::agent_chains::tests::{anthropic, endpoints, offer, state_dir, GMAIL, GMAIL_ORG, PERSON, PERSONAL_ORG};
use crate::agent_chains::{Owner, Read, Slot, Stores, Who, sync_once};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const PERSONAL: &str = "claude:fixture@example.invalid";
const OTHER: &str = "claude:other@example.invalid";

#[derive(Default)]
struct Memory(std::sync::Mutex<HashMap<String, Value>>);

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
    fn same(&self, _: &Slot) -> bool { true }
}

impl Memory {
    fn token(&self, id: &str) -> Option<String> {
        self.0.lock().unwrap().get(id)?["claudeAiOauth"]["accessToken"].as_str().map(str::to_string)
    }
}

fn login(access: &str, refresh: Option<&str>) -> Value {
    let o = offer(access, refresh.unwrap_or("unused"), 7 * 3_600_000);
    let mut doc = json!({"claudeAiOauth":{"accessToken":o.access_token,"expiresAt":o.expires_at,
        "scopes":o.scopes,"subscriptionType":"max"}});
    if let Some(refresh) = refresh { doc["claudeAiOauth"]["refreshToken"] = json!(refresh); }
    doc
}

fn slot(id: &str) -> Slot {
    Slot { id: id.into(), store: None, table_key: None, bench: false, stamped: None }
}

struct Device {
    token: String,
    paths: local::Paths,
    store: Memory,
}

impl Device {
    fn credential(&self, relay: &str) -> crate::device_auth::DeviceCred {
        crate::device_auth::DeviceCred { relay: relay.into(), account: "one".into(), device_id: "rig".into(),
            token: self.token.clone(), display_name: None }
    }

    async fn sync_settings(&self, relay: &str) {
        let apply = |request: PendingApply| crate::account_sync::apply_files(&request);
        crate::account_sync::synchronize(&reqwest::Client::new(), &self.credential(relay),
            crate::device_auth::Stamp::fixture(), self.paths.clone(), &|| Ok(()), &apply).await.unwrap();
    }

    fn settings(&self) -> Value {
        local::read_json(&self.paths.settings, json!({})).unwrap()
    }
}

async fn gateway() -> (String, Gate, String, String) {
    let dir = state_dir();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    accounts.accounts.insert("one".into(), crate::relay_auth::Account {
        pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture", 1), disabled: false, created: 1, login: None,
    });
    crate::relay_auth::save_accounts(&dir.join("accounts.json"), &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(dir.join("accounts.json")));
    gate.agent_chains = None;
    let mut service = crate::agent_chains::Service::open(Some(&dir.join("state.json"))).unwrap();
    let (provider, _) = anthropic().await;
    Arc::get_mut(&mut service).unwrap().endpoints = endpoints(&provider);
    gate.agent_chains = Some(service);
    let (book, mini) = (crate::relay_auth::new_token(), crate::relay_auth::new_token());
    for (id, token) in [("dev_book", &book), ("dev_mini", &mini)] {
        gate.devices.lock().unwrap().insert(id.into(), DeviceRec {
            token_hash: crate::relay_auth::token_hash(token), account: "one".into(), kind: "desktop".into(),
            machine_id: Some(format!("machine-{id}")), label: id.into(), created: 1, last_seen: 1, revoked_at: None,
        });
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let served = gate.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    (format!("http://{address}"), gate, book, mini)
}

fn device(root: &std::path::Path, name: &str, token: String, settings: Value, docs: &[(&str, Value)]) -> Device {
    let paths = local::Paths {
        settings: root.join(name).join("settings.json"),
        machines: root.join(name).join("machines.json"),
        binding: root.join(name).join("binding.json"),
    };
    local::write_private(&paths.settings, &settings).unwrap();
    let store = Memory(std::sync::Mutex::new(docs.iter().map(|(id, doc)| (id.to_string(), doc.clone())).collect()));
    Device { token, paths, store }
}

#[tokio::test]
async fn the_mini_follows_the_laptops_verified_choice_into_a_new_slot_without_touching_its_gmail_slots() {
    let (relay, gate, book_token, mini_token) = gateway().await;
    let profile = gate.agent_chains.as_ref().unwrap().endpoints.profile.clone();
    let root = std::env::temp_dir().join(format!("kasa-recovery-rig-{}", uuid::Uuid::new_v4()));
    let book = device(&root, "book", book_token, json!({"claude_account":"acct-5"}),
        &[("acct-5", login("access-personal-book", Some("refresh-book")))]);
    // The mini as the old build left it: acct-6 is named for the personal account but holds Gmail.
    let mini = device(&root, "mini", mini_token, json!({"claude_account":"acct-6"}),
        &[("acct-1", login("access-gmail-1", Some("refresh-gmail"))), ("acct-6", login("access-gmail-6", None))]);
    let gmail = Who::new(GMAIL, GMAIL_ORG).unwrap();
    let ledger = crate::agent_chains::recovery::Ledger::default();
    let book_gw = crate::agent_chains::Gateway::new(&relay, &book.token).with_profile(&profile);
    let mini_gw = crate::agent_chains::Gateway::new(&relay, &mini.token).with_profile(&profile);

    // Both devices hand over their own logins; the gateway now holds the personal and the Gmail chain.
    let (book_learned, _) = sync_once(&book_gw, &[slot("acct-5")], &BTreeMap::new(), &BTreeMap::new(), &book.store, &|| true, &ledger).await.unwrap();
    assert_eq!(book_learned.keys["acct-5"], PERSONAL);
    // The old build mapped acct-6 to the personal chain after an outside login; its vault is Gmail.
    let polluted: BTreeMap<String, String> = [("acct-6".to_string(), PERSONAL.to_string())].into();
    let mini_owners: BTreeMap<String, Owner> = [("acct-6".to_string(), Owner {
        key: OTHER.into(), fingerprint: crate::agent_chains::fingerprint("access-gmail-6"), who: gmail.clone() })].into();
    let (mini_learned, chains) = sync_once(&mini_gw, &[slot("acct-1"), slot("acct-6")], &polluted, &mini_owners, &mini.store, &|| true, &ledger).await.unwrap();
    assert_eq!(mini_learned.keys["acct-1"], OTHER);
    assert_eq!(mini.store.token("acct-6").as_deref(), Some("access-gmail-6"), "the verified Gmail vault was overwritten with the personal chain");
    assert!(chains.iter().any(|chain| chain.key == PERSONAL && chain.state == "ok"));

    // The person selects the personal slot on the laptop once (the official explicit choice); it is published.
    book.sync_settings(&relay).await;
    mini.sync_settings(&relay).await;
    let owner = local::account_key(&relay, "one");
    let seen = local::load_bindings(&book.paths.binding).unwrap().accounts[&owner].remote.revision;
    crate::account_sync::choose_as(&book.paths, &owner, seen, PERSON, PERSONAL_ORG,
        "Fixture@Example.invalid", "fixture@example.invalid's Organization").unwrap();
    book.sync_settings(&relay).await;
    mini.sync_settings(&relay).await;
    let canonical = choice::owned(&mini.settings(), &owner).cloned().expect("the mini did not receive the choice");
    assert!(choice::rev(&canonical).is_some());

    // The mini has no slot with the chosen UUIDs: both of its slots prove to be Gmail.
    for id in ["acct-1", "acct-6"] {
        let found = mini_gw.prove(&mini.store.token(id).unwrap()).await.unwrap();
        assert_eq!(found, gmail);
    }
    // A normal held chain exists for that account, so the official adopt prepares a new slot (map only, no token).
    let key = crate::agent_accounts::account_key("claude", canonical["email"].as_str().unwrap(), canonical["org"].as_str().unwrap(), "");
    assert_eq!(key, PERSONAL);
    let mut map: BTreeMap<String, String> = mini_learned.keys.clone();
    map.insert("acct-6".into(), PERSONAL.into());
    map.insert("acct-7".into(), PERSONAL.into());
    let mut owners = mini_owners.clone();
    owners.extend(mini_learned.owners.clone());
    let slots = [slot("acct-1"), slot("acct-6"), slot("acct-7")];
    let (adopted, _) = sync_once(&mini_gw, &slots, &map, &owners, &mini.store, &|| true, &ledger).await.unwrap();
    let received = mini.store.token("acct-7").expect("the adopted slot was not filled");
    // Verify the received token's UUIDs before selecting it.
    let proven = mini_gw.prove(&received).await.unwrap();
    assert_eq!((proven.account_uuid.as_str(), proven.organization_uuid.as_str()), (PERSON, PERSONAL_ORG));
    assert_eq!(adopted.owners["acct-7"].who, proven);
    local::edit_json(&mini.paths.settings, json!({}), |v| { v["claude_account"] = json!("acct-7"); Ok(()) }).unwrap();

    // Nothing else moved: both Gmail slots keep Gmail, the laptop keeps its slot, and following published nothing.
    assert_eq!(mini.store.token("acct-6").as_deref(), Some("access-gmail-6"));
    assert_eq!(mini_gw.prove(&mini.store.token("acct-1").unwrap()).await.unwrap(), gmail);
    assert!(book.store.token("acct-5").unwrap().starts_with("access-personal"));
    mini.sync_settings(&relay).await;
    let relay_choice = gate.account_sync.get("one").unwrap().settings.get(choice::KEY).cloned().unwrap();
    assert_eq!(choice::intent(&relay_choice), choice::intent(&canonical), "the following device re-published the choice");
    assert!(choice::sent(&mini.settings()).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}
