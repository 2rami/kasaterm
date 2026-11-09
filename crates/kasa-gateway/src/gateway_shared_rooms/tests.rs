use super::*;
use super::super::tests::{account_gate, get_json, post, spawn_relay};
use sha2::Digest;

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kasa-shared-rooms-http-{}", uuid::Uuid::new_v4()));
        drop(account_gate(&path));
        let file = path.join("relay-accounts.json");
        let mut accounts = kasa_identity::relay_auth::load_accounts(&file);
        let sample = accounts.accounts.remove("geno").unwrap();
        for name in ["alice", "bob", "carol", "dave"] { accounts.accounts.insert(name.into(), sample.clone()); }
        kasa_identity::relay_auth::save_accounts(&file, &accounts).unwrap();
        Self(path)
    }
    fn gate(&self) -> Gate { Gate::new(Some(self.0.join("relay-state.json"))) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

async fn login(addr: std::net::SocketAddr, account: &str, kind: &str) -> String {
    let body = json!({"account":account,"password":"correct horse","kind":kind,"machine_id":format!("{account}-{kind}-machine")});
    let (status, value) = post(addr, "/relay/login", None, body).await;
    assert_eq!(status, 200, "{value}");
    value["token"].as_str().unwrap().into()
}

async fn call(addr: std::net::SocketAddr, method: reqwest::Method, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let mut request = reqwest::Client::new().request(method, format!("http://{addr}{path}")).bearer_auth(token);
    if let Some(body) = body { request = request.json(&body); }
    let reply = request.send().await.unwrap();
    (reply.status().as_u16(), reply.json().await.unwrap_or_default())
}

async fn create_room(addr: std::net::SocketAddr, token: &str, allow_fork: bool) -> String {
    let (status, value) = post(addr, "/relay/shared-rooms", Some(token), json!({"title":"Pairing","allow_fork":allow_fork})).await;
    assert_eq!(status, 200, "{value}");
    value["room"]["id"].as_str().unwrap().into()
}

async fn invite(addr: std::net::SocketAddr, token: &str, room: &str, max_uses: u32) -> String {
    let (status, value) = post(addr, &format!("/relay/shared-rooms/{room}/invites"), Some(token), json!({"max_uses":max_uses})).await;
    assert_eq!(status, 200, "{value}");
    value["invite"]["token"].as_str().unwrap().into()
}

async fn accept(addr: std::net::SocketAddr, token: &str, invite: &str) -> (u16, Value) {
    post(addr, "/relay/shared-room-invites/accept", Some(token), json!({"token":invite})).await
}

async fn publish(addr: std::net::SocketAddr, token: &str, room: &str) -> (u16, Value) {
    post(addr, &format!("/relay/shared-rooms/{room}/sessions"), Some(token),
        json!({"source_key":"/Users/alice/.claude/projects/app/s.jsonl","title":"Debugging","harness":"claude"})).await
}

fn append_body(session: &Value, raw: &str) -> Value {
    let cursor = session["cursor"].as_u64().unwrap();
    json!({"expected_revision":session["revision"],"generation":session["generation"],"from":cursor,"next":cursor + 10,
        "reset":false,"raw":raw,"caught_up":true})
}

#[tokio::test]
async fn members_read_only_what_owners_publish_and_owners_alone_manage() {
    let fixture = Fixture::new();
    let addr = spawn_relay(fixture.gate()).await;
    let desk = login(addr, "alice", "desktop").await;
    let phone = login(addr, "alice", "phone").await;
    let bob = login(addr, "bob", "phone").await;
    let bob_desk = login(addr, "bob", "desktop").await;
    let carol = login(addr, "carol", "phone").await;
    assert_eq!(get_json(addr, "/relay/shared-rooms", "invalid").await.0, 401);
    let spoof = json!({"title":"Pairing","allow_fork":false,"owner_account":"bob"});
    assert_eq!(post(addr, "/relay/shared-rooms", Some(&phone), spoof).await.0, 400);
    let room = create_room(addr, &phone, false).await;
    let path = format!("/relay/shared-rooms/{room}");
    assert_eq!(get_json(addr, &path, &bob).await.0, 404);
    let retry = json!({"title":"Retry","allow_fork":false,"request_id":"form-7f3a-0001"});
    let (_, created) = post(addr, "/relay/shared-rooms", Some(&phone), retry.clone()).await;
    assert_eq!(post(addr, "/relay/shared-rooms", Some(&desk), retry).await.1["room"]["id"], created["room"]["id"]);
    let changed = json!({"title":"Retry","allow_fork":true,"request_id":"form-7f3a-0001"});
    assert_eq!(post(addr, "/relay/shared-rooms", Some(&phone), changed).await.0, 409);
    assert_eq!(call(addr, reqwest::Method::DELETE, &format!("/relay/shared-rooms/{}", created["room"]["id"].as_str().unwrap()), &phone, None).await.0, 200);
    let code = invite(addr, &phone, &room, 1).await;
    let (status, joined) = accept(addr, &bob, &code).await;
    assert_eq!((status, joined["room"]["my_role"].as_str()), (200, Some("member")), "{joined}");
    assert_eq!(publish(addr, &phone, &room).await.1["error"], "desktop_only");
    assert_eq!(publish(addr, &bob_desk, &room).await.0, 403);
    let spoofed = json!({"source_key":"s","title":"t","harness":"claude","publisher_machine_id":"bob-desktop-machine"});
    assert_eq!(post(addr, &format!("{path}/sessions"), Some(&desk), spoofed).await.0, 400);
    let (status, published) = publish(addr, &desk, &room).await;
    assert_eq!(status, 200, "{published}");
    let session = published["session"].clone();
    assert_eq!(session["publisher_machine_id"], "alice-desktop-machine");
    let append = format!("{path}/sessions/{}/append", session["id"].as_str().unwrap());
    assert_eq!(post(addr, &append, Some(&bob_desk), append_body(&session, "x\n")).await.0, 403);
    let (status, appended) = post(addr, &append, Some(&desk), append_body(&session, "첫 줄\n")).await;
    assert_eq!(status, 200, "{appended}");
    assert_eq!(post(addr, &append, Some(&desk), append_body(&session, "stale\n")).await.1["error"], "shared_conflict");
    let content = format!("{path}/sessions/{}/content?generation={}&offset=0&end={}",
        session["id"].as_str().unwrap(), session["generation"].as_str().unwrap(), appended["session"]["bytes"]);
    assert_eq!(get_json(addr, &content, &bob).await.1["content"]["raw"], "첫 줄\n");
    assert_eq!(get_json(addr, &content, &carol).await.0, 404);
    invite(addr, &phone, &room, 1).await;
    let (_, member_view) = get_json(addr, &path, &bob).await;
    let member_view = &member_view["room"];
    assert_eq!((member_view["invites"].as_array().unwrap().len(), &member_view["sessions"][0]["source_key"]), (0, &json!("")));
    let (_, owner_view) = get_json(addr, "/relay/shared-rooms", &phone).await;
    assert_eq!(owner_view["rooms"][0]["invites"].as_array().unwrap().len(), 1);
    assert_eq!(owner_view["rooms"][0]["sessions"][0]["cursor"], 10);
    let rename = json!({"expected_revision":member_view["revision"],"title":"Mine now"});
    assert_eq!(call(addr, reqwest::Method::PATCH, &path, &bob, Some(rename)).await.0, 403);
    assert_eq!(call(addr, reqwest::Method::DELETE, &path, &bob, None).await.0, 403);
    assert_eq!(call(addr, reqwest::Method::DELETE, &format!("{path}/members/alice"), &bob, None).await.0, 403);
    let (status, left) = call(addr, reqwest::Method::DELETE, &format!("{path}/members/bob"), &bob, None).await;
    assert_eq!((status, &left["room"]), (200, &Value::Null));
    assert_eq!(get_json(addr, &content, &bob).await.0, 404);
    let restarted = spawn_relay(fixture.gate()).await;
    assert_eq!(get_json(restarted, &content, &phone).await.1["content"]["raw"], "첫 줄\n");
    assert_eq!(post(addr, "/relay/logout", Some(&phone), json!({})).await.0, 200);
    assert_eq!(get_json(addr, &content, &phone).await.0, 401);
}

#[tokio::test]
async fn a_single_use_invite_admits_exactly_one_of_two_concurrent_accounts() {
    let fixture = Fixture::new();
    let addr = spawn_relay(fixture.gate()).await;
    let alice = login(addr, "alice", "phone").await;
    let bob = login(addr, "bob", "phone").await;
    let carol = login(addr, "carol", "phone").await;
    let dave = login(addr, "dave", "phone").await;
    let room = create_room(addr, &alice, false).await;
    let code = invite(addr, &alice, &room, 1).await;
    let (first, second) = tokio::join!(accept(addr, &bob, &code), accept(addr, &carol, &code));
    let mut statuses = [first.0, second.0];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 410]);
    let (status, reused) = accept(addr, &dave, &code).await;
    assert_eq!((status, reused["error"].as_str()), (410, Some("invite_unavailable")));
    let (_, created) = post(addr, &format!("/relay/shared-rooms/{room}/invites"), Some(&alice), json!({})).await;
    let revoke = format!("/relay/shared-rooms/{room}/invites/{}", created["invite"]["id"].as_str().unwrap());
    assert_eq!(call(addr, reqwest::Method::DELETE, &revoke, &bob, None).await.0, if first.0 == 200 { 403 } else { 404 });
    assert_eq!(call(addr, reqwest::Method::DELETE, &revoke, &alice, None).await.0, 200);
    assert_eq!(accept(addr, &dave, created["invite"]["token"].as_str().unwrap()).await.0, 410);
    assert_eq!(accept(addr, &dave, "ksr1.garbage").await.0, 400);
}

#[tokio::test]
async fn revocation_while_the_append_body_arrives_commits_nothing() {
    let fixture = Fixture::new();
    let addr = spawn_relay(fixture.gate()).await;
    let desk = login(addr, "alice", "desktop").await;
    let phone = login(addr, "alice", "phone").await;
    let room = create_room(addr, &phone, false).await;
    let session = publish(addr, &desk, &room).await.1["session"].clone();
    let body = serde_json::to_vec(&append_body(&session, "never committed\n")).unwrap();
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    let token = desk.clone();
    let path = format!("http://{addr}/relay/shared-rooms/{room}/sessions/{}/append", session["id"].as_str().unwrap());
    let request = tokio::spawn(async move {
        reqwest::Client::new().post(path).bearer_auth(token).header("content-type", "application/json")
            .body(reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(receive))).send().await.unwrap()
    });
    send.send(Ok(body[..body.len() / 2].to_vec())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(post(addr, "/relay/logout", Some(&desk), json!({})).await.0, 200);
    let _ = send.send(Ok(body[body.len() / 2..].to_vec())).await;
    drop(send);
    assert_eq!(request.await.unwrap().status(), 401);
    let (_, view) = get_json(addr, &format!("/relay/shared-rooms/{room}"), &phone).await;
    assert_eq!((&view["room"]["sessions"][0]["bytes"], &view["room"]["sessions"][0]["revision"]), (&json!(0), &json!(0)));
}

fn checkpoint(session: &Value, line: &str) -> Value {
    let jsonl = format!("{line}\n");
    let sha = format!("{:x}", sha2::Sha256::digest(jsonl.as_bytes()));
    json!({"expected_generation":session["generation"],"expected_revision":session["revision"],
        "checkpoint":{"format_version":1,"native":{"harness":"claude","parent_session_id":"0b4d3f5e-1111-2222-3333-444455556666",
        "boundary_id":"msg-42","boundary_label":"after the tests","native_version":"2.1.0","jsonl":jsonl,"sha256":sha},
        "captured_at_ms":1_000,"display_raw":"display\n","project":null,"omissions":[]}})
}

#[tokio::test]
async fn fork_copies_belong_to_their_recipient_and_outlive_the_room() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    let addr = spawn_relay(gate.clone()).await;
    let desk = login(addr, "alice", "desktop").await;
    let phone = login(addr, "alice", "phone").await;
    let bob = login(addr, "bob", "phone").await;
    let carol = login(addr, "carol", "phone").await;
    let room = create_room(addr, &phone, true).await;
    accept(addr, &bob, &invite(addr, &phone, &room, 2).await).await;
    let session = publish(addr, &desk, &room).await.1["session"].clone();
    let base = format!("/relay/shared-rooms/{room}/sessions/{}", session["id"].as_str().unwrap());
    assert_eq!(post(addr, &format!("{base}/checkpoints"), Some(&phone), checkpoint(&session, r#"{"uuid":"a"}"#)).await.1["error"], "desktop_only");
    let (status, published) = post(addr, &format!("{base}/checkpoints"), Some(&desk), checkpoint(&session, r#"{"uuid":"a"}"#)).await;
    assert_eq!(status, 200, "{published}");
    let mut stale = checkpoint(&session, r#"{"uuid":"older"}"#);
    stale["checkpoint"]["captured_at_ms"] = json!(500);
    assert_eq!(post(addr, &format!("{base}/checkpoints"), Some(&desk), stale).await.1["error"], "shared_conflict");
    let bare = checkpoint(&session, r#"{"uuid":"bare"}"#)["checkpoint"].clone();
    assert_eq!(post(addr, &format!("{base}/checkpoints"), Some(&desk), bare).await.0, 400, "the CAS fields are required");
    let request = json!({"checkpoint_id":published["checkpoint"]["id"],"request_id":"fork-request-1"});
    assert_eq!(post(addr, &format!("{base}/fork"), Some(&carol), request.clone()).await.0, 404);
    let (status, fork) = post(addr, &format!("{base}/fork"), Some(&bob), request.clone()).await;
    assert_eq!(status, 200, "{fork}");
    assert_eq!(post(addr, &format!("{base}/fork"), Some(&bob), request.clone()).await.1["fork"]["id"], fork["fork"]["id"]);
    let copy = format!("/relay/shared-forks/{}", fork["fork"]["id"].as_str().unwrap());
    assert_eq!(get_json(addr, &copy, &carol).await.0, 404);
    assert_eq!(get_json(addr, &copy, &phone).await.0, 404);
    assert_eq!(get_json(addr, "/relay/shared-forks", &bob).await.1["forks"][0]["id"], fork["fork"]["id"]);
    assert!(get_json(addr, "/relay/shared-forks", &carol).await.1["forks"].as_array().unwrap().is_empty());
    assert_eq!(call(addr, reqwest::Method::DELETE, &copy, &carol, None).await.0, 404);
    assert_eq!(call(addr, reqwest::Method::DELETE, &format!("/relay/shared-rooms/{room}"), &phone, None).await.0, 200);
    let downloads = gate.shared_rooms.clone().unwrap().downloads.clone();
    let budget = downloads.available_permits();
    let held = downloads.clone().acquire_many_owned(u32::try_from(budget).unwrap()).await.unwrap();
    assert_eq!(get_json(addr, &copy, &bob).await.1["error"], "shared_busy", "no reservation, no copy in memory");
    drop(held);
    let (status, kept) = get_json(addr, &copy, &bob).await;
    assert_eq!((status, kept["fork"]["checkpoint"]["native"]["jsonl"].as_str()), (200, Some("{\"uuid\":\"a\"}\n")));
    for _ in 0..40 {
        if downloads.available_permits() == budget { break; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(downloads.available_permits(), budget, "a finished download returns its reservation");
    assert_eq!(get_json(addr, &format!("/relay/shared-rooms/{room}"), &bob).await.0, 404);
    assert_eq!(call(addr, reqwest::Method::DELETE, &copy, &bob, None).await.0, 200);
    let room = create_room(addr, &phone, true).await;
    accept(addr, &bob, &invite(addr, &phone, &room, 1).await).await;
    let session = publish(addr, &desk, &room).await.1["session"].clone();
    let base = format!("/relay/shared-rooms/{room}/sessions/{}", session["id"].as_str().unwrap());
    let first = post(addr, &format!("{base}/checkpoints"), Some(&desk), checkpoint(&session, r#"{"uuid":"b"}"#)).await.1;
    let resent = post(addr, &format!("{base}/checkpoints"), Some(&desk), checkpoint(&session, r#"{"uuid":"b"}"#)).await.1;
    assert_eq!(first["checkpoint"]["id"], resent["checkpoint"]["id"]);
    assert_eq!(call(addr, reqwest::Method::DELETE, &base, &bob, None).await.0, 403);
    assert_eq!(call(addr, reqwest::Method::DELETE, &base, &phone, None).await.0, 200);
    assert_eq!(get_json(addr, &format!("{base}/content"), &bob).await.0, 404);
    assert_eq!(post(addr, &format!("{base}/append"), Some(&desk), append_body(&session, "late\n")).await.0, 404);
    assert!(get_json(addr, "/relay/shared-forks", &bob).await.1["forks"].as_array().unwrap().is_empty());
    assert_eq!(get_json(addr, "/relay/shared-forks", "invalid").await.0, 401);
}
