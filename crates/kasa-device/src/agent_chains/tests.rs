use super::*;
use super::testing::{seen_calls, PERSON, PERSONAL_ORG, service, offer, HOUR, slot_doc, access, Docs};

#[test]
fn reconnect_reuses_only_a_still_registered_chain_slot() {
    let key = "claude:fixture@example.invalid";
    let map = BTreeMap::from([
        ("removed".into(), key.into()),
        ("acct-1".into(), key.into()),
        ("acct-2".into(), "claude:other@example.invalid".into()),
    ]);
    for _ in 0..3 {
        assert_eq!(registered_slot_in(key, ["acct-2", "acct-1"].into_iter(), &map).as_deref(), Some("acct-1"));
    }
    assert_eq!(registered_slot_in(key, ["acct-2"].into_iter(), &map), None);
}

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

/// 로그인 수명은 갱신으로 안 는다 — 끝나기 전에 다시 로그인한 사슬은 멀쩡한 옛 사슬을 갈아 끼운다. 수명이 더 짧은
/// 옛 로그인은 지금 사슬을 못 밀어낸다.
#[tokio::test]
async fn a_fresher_login_renews_a_chain_close_to_its_login_expiry() {
    let (service, _, dir) = service().await;
    let key = "claude:fixture@example.invalid";
    service.seal("one", "dev_a", offer("access-personal-1", "refresh-1", 7 * HOUR)).await.unwrap();
    let listed = service.list("one").await.unwrap();
    let first_expiry = listed["chains"][0]["login_expires_at"].as_u64().unwrap();
    assert!(first_expiry > now_ms() + 27 * 24 * HOUR, "a fresh login reads as about four weeks");

    let mut stale = offer("access-personal-b", "refresh-b", 7 * HOUR);
    stale.refresh_token_expires_at = now_ms() + 3 * 24 * HOUR;
    assert_eq!(service.seal("one", "dev_b", stale).await.unwrap()["held"], true, "an older login replaced a fresher chain");

    let mut book = service.load("one").unwrap();
    let chain = book.get_mut(key).unwrap();
    chain.sealed_at = now_ms() - 26 * 24 * HOUR;
    chain.refresh_expires_at = now_ms() + 2 * 24 * HOUR;
    service.save("one", &book).unwrap();
    let renewed = service.seal("one", "dev_b", offer("access-personal-2", "refresh-2", 7 * HOUR)).await.unwrap();
    assert_eq!(renewed["held"], false);
    assert_eq!(renewed["access"]["accessToken"], "access-personal-2");
    assert_eq!(service.load("one").unwrap()[key].refresh_token, "refresh-2");
    let audit = std::fs::read_to_string(dir.join("agent-chains/audit.jsonl")).unwrap();
    assert!(audit.contains("renewed"));
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
    let (refreshed, notices, changed) = service.refresh_due().await;
    assert_eq!(refreshed, 1);
    assert_eq!(changed, vec!["one"]);
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
    assert_eq!(personal.lineage.len(), 1);
    assert_eq!(personal.lineage[0].fingerprint, fingerprint("access-personal-1"));
    assert_eq!(personal.lineage[0].generation, personal.generation);
    let team = &book["claude:fixture team"];
    assert!(team.broken && team.refresh_token.is_empty() && team.access_token.is_empty());
    let broken_generation = team.generation.clone();

    let listed = service.list("one").await.unwrap();
    assert!(!listed.to_string().contains("access-personal-1"), "lineage leaked the retired token");
    let states: Vec<(&str, bool)> = listed["chains"].as_array().unwrap().iter()
        .map(|c| (c["state"].as_str().unwrap(), c["access"].is_null())).collect();
    assert_eq!(states, vec![("reconnect_required", true), ("ok", false)]);

    // Not due again for hours; a broken chain is never retried.
    assert_eq!(service.refresh_due().await.0, 0);
    assert_eq!(seen_calls(&calls, "token").len(), 2);

    // Signing in again anywhere replaces the broken chain.
    let again = service.seal("one", "d", offer("access-team-2", "refresh-t2", 7 * HOUR)).await.unwrap();
    assert_eq!(again["held"], false);
    let book = service.load("one").unwrap();
    assert!(!book["claude:fixture team"].broken);
    assert_ne!(book["claude:fixture team"].generation, broken_generation);
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

#[tokio::test]
async fn server_lineage_covers_two_pull_cycles_but_not_grace_or_original_expiry() {
    let (service, _, _) = service().await;
    service.seal("one", "d", offer("access-personal-1", "refresh-1", 7 * HOUR)).await.unwrap();
    let key = "claude:fixture@example.invalid";
    let clock = now_ms();
    let mut book = service.load("one").unwrap();
    let chain = book.get_mut(key).unwrap();
    chain.updated = clock - 5 * 60_000 - 30_000;
    chain.lineage = vec![Spent {
        fingerprint: fingerprint("retired-token"),
        expires_at: clock + HOUR,
        generation: chain.generation.clone(),
    }];
    service.save("one", &book).unwrap();
    let listed = service.list("one").await.unwrap();
    assert_eq!(listed["chains"][0]["lineage"].as_array().unwrap().len(), 1);

    let mut book = service.load("one").unwrap();
    book.get_mut(key).unwrap().updated = clock.saturating_sub(LINEAGE_TTL_MS + 1);
    service.save("one", &book).unwrap();
    assert!(service.list("one").await.unwrap()["chains"][0]["lineage"].as_array().unwrap().is_empty());

    let mut book = service.load("one").unwrap();
    let chain = book.get_mut(key).unwrap();
    chain.updated = clock;
    chain.lineage[0].expires_at = clock.saturating_sub(1);
    service.save("one", &book).unwrap();
    assert!(service.list("one").await.unwrap()["chains"][0]["lineage"].as_array().unwrap().is_empty());
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
    let (_, notices, _) = service.refresh_due().await;
    assert_eq!(notices.len(), 1);
    assert!(notices[0].title.contains("막혔어요"));
    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| c.retry_at = 0);
    service.save("one", &book).unwrap();
    assert!(service.refresh_due().await.1.is_empty(), "the same stall alerted twice");
}

#[test]
fn a_store_commit_requires_the_complete_original_document() {
    let old = slot_doc("old", None, HOUR as i64);
    let mut changed = old.clone();
    changed["claudeAiOauth"]["refreshToken"] = json!("new-phone-refresh");
    assert!(matches_document(Read::Present(old.clone()), Some(&old)));
    assert!(!matches_document(Read::Present(changed), Some(&old)));
    let mut changed = old.clone();
    changed["mcpOAuth"]["server|abc"]["accessToken"] = json!("new-connector-token");
    assert!(!matches_document(Read::Present(changed), Some(&old)));
    assert!(!matches_document(Read::Unknown, None));
    assert!(!matches_document(Read::Absent, Some(&old)));
    assert!(matches_document(Read::Absent, None));
    assert!(!Local.compare_and_write(&Slot {id:"fixture".into(),store:None,table_key:None, bench: false, stamped: None}, None, &old));
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
        .map(|id| Slot { id: id.to_string(), store: None, table_key: None, bench: false, stamped: None }).collect();
    let map: BTreeMap<String, String> = [("a", "claude:x@y"), ("b", "claude:x@y"), ("own", "claude:x@y"),
        ("fresh", "claude:z@y"), ("team", "claude:team")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let down = device_alerts(&slots, &map, &docs, None, now);
    assert_eq!(down.keys().collect::<Vec<_>>(), vec!["stalled|claude:x@y"], "one alert per account, none for healthy or own chains");
    assert!(down["stalled|claude:x@y"].body.contains("관문에 닿지 않아"));
    let chains = vec![Held { key: "claude:team".into(), state: "reconnect_required".into(), ..Default::default() }];
    let up = device_alerts(&slots, &map, &docs, Some(&chains), now);
    assert!(up.contains_key("broken|claude:team") && up["stalled|claude:x@y"].body.contains("갱신하지 못해"));
}

#[test]
fn stale_slot_mappings_and_current_identity_do_not_manage_a_deleted_account() {
    let slots = vec![Slot {
        id: "current".into(),
        store: None,
        table_key: Some("claude:fixture@example.invalid".into()),
        bench: false, stamped: None,
    }];
    let stale = BTreeMap::from([("deleted".into(), "claude:fixture@example.invalid".into())]);
    assert!(managed_keys(&slots, &stale, &BTreeMap::new()).is_empty());
}

#[cfg(unix)]
#[test]
fn bounded_child_output_drains_more_than_a_pipe_buffer_before_exit() {
    let child = kasa_socket::process::no_window_command("sh")
        .args(["-c", "i=0; while [ \"$i\" -lt 8192 ]; do printf 0123456789abcdef; i=$((i+1)); done"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let (status, output) = bounded_child_output(child, Duration::from_secs(2), 1024 * 1024).unwrap();
    assert!(status.success());
    assert_eq!(output.len(), 8192 * 16);
}

#[cfg(unix)]
#[test]
fn bounded_child_output_times_out_kills_and_reaps_the_child() {
    let child = kasa_socket::process::no_window_command("sh")
        .args(["-c", "while :; do :; done"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let error = bounded_child_output(child, Duration::from_millis(50), 1024).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let alive = unsafe { libc::kill(pid as i32, 0) };
    assert_eq!(alive, -1, "timed-out child was not reaped");
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[tokio::test]
async fn chains_carry_provider_uuids_and_older_chains_gain_them_on_the_next_refresh() {
    let (service, _, _) = service().await;
    let sealed = service.seal("one", "d", offer("access-personal-1", "refresh-1", 7 * HOUR)).await.unwrap();
    assert_eq!((sealed["account_uuid"].as_str(), sealed["organization_uuid"].as_str()), (Some(PERSON), Some(PERSONAL_ORG)));
    let listed = service.list("one").await.unwrap();
    assert_eq!(listed["chains"][0]["account_uuid"], PERSON);

    let twin = service.seal("one", "d", offer("access-twin-1", "refresh-twin", 7 * HOUR)).await;
    assert_eq!(twin.err(), Some(Error::Conflict), "a held chain of another provider account was handed out");

    // A chain sealed before this field existed: the gateway learns its owner from the rotated token.
    let mut book = service.load("one").unwrap();
    book.values_mut().for_each(|c| {
        c.account_uuid.clear();
        c.organization_uuid.clear();
        c.sealed_at = 0;
        c.expires_at = now_ms() + HOUR;
    });
    service.save("one", &book).unwrap();
    assert!(service.list("one").await.unwrap()["chains"][0].get("account_uuid").is_none());
    assert_eq!(service.refresh_due().await.0, 1);
    let chain = &service.load("one").unwrap()["claude:fixture@example.invalid"];
    assert_eq!((chain.account_uuid.as_str(), chain.organization_uuid.as_str()), (PERSON, PERSONAL_ORG));
}

/// 다시 로그인할 때를 고르는 도구가 읽는 슬롯별 상태 — 작업대는 슬롯의 계정을 알려 주지 못하니 뺀다.
#[test]
fn each_mapped_slot_reports_its_chain_and_login_expiry() {
    let slot = |id: &str, bench: bool| Slot { id: id.into(), store: None, table_key: None, bench, stamped: None };
    let map = BTreeMap::from([
        ("acct-1".to_string(), "claude:a@example.invalid".to_string()),
        ("acct-2".to_string(), "claude:gone@example.invalid".to_string()),
        ("bench".to_string(), "claude:a@example.invalid".to_string()),
    ]);
    let chains = vec![Held {
        key: "claude:a@example.invalid".into(),
        email: "a@example.invalid".into(),
        state: "ok".into(),
        login_expires_at: 42,
        ..Held::default()
    }];
    let logins = chain_logins(&[slot("acct-1", false), slot("acct-2", false), slot("bench", true), slot("acct-9", false)], &map, &chains);
    assert_eq!(logins.len(), 2);
    assert_eq!((logins[0].slot.as_str(), logins[0].state.as_str(), logins[0].login_expires_at), ("acct-1", "ok", 42));
    assert_eq!((logins[1].slot.as_str(), logins[1].state.as_str()), ("acct-2", "missing"));
}
