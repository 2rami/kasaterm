use super::*;
use super::super::tests::{account_gate, get_json, post, spawn_relay};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("kasa-conversations-http-{}", uuid::Uuid::new_v4()));
        drop(account_gate(&path));
        let file = path.join("relay-accounts.json");
        let mut accounts = crate::relay_auth::load_accounts(&file);
        let sample = accounts.accounts.remove("geno").unwrap();
        accounts.accounts.insert("alice".into(), sample.clone());
        accounts.accounts.insert("bob".into(), sample);
        crate::relay_auth::save_accounts(&file, &accounts).unwrap();
        Self(path)
    }
    fn gate(&self) -> Gate { Gate::new(Some(self.0.join("relay-state.json"))) }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

async fn login(addr: std::net::SocketAddr, account: &str, kind: &str) -> String {
    let (status, value) = post(addr, "/relay/login", None, json!({"account":account,"password":"correct horse", "kind":kind,"machine_id":format!("archive-{kind}")})).await;
    assert_eq!(status, 200, "{value}"); value["token"].as_str().unwrap().into()
}

fn update() -> Value {
    json!({"source":"session.jsonl","surface_key":"pane-key","title":"Stored conversation", "name":"Assistant", "harness":"claude", "cwd":"/project",
        "expected_revision":0,"from":0,"next":900,"reset":true,"caught_up":true,"raw":"first message\n"})
}

async fn enable(addr: std::net::SocketAddr, token: &str) {
    let reply = reqwest::Client::new().put(format!("http://{addr}/relay/conversations/settings"))
        .bearer_auth(token).json(&json!({"enabled":true})).send().await.unwrap();
    assert_eq!(reply.status(), 200);
}

#[tokio::test]
async fn archive_routes_are_durable_account_scoped_and_independent_of_live_desktops() {
    let fixture = Fixture::new(); let addr = spawn_relay(fixture.gate()).await;
    let desktop = login(addr, "alice", "desktop").await;
    let phone = login(addr, "alice", "phone").await;
    let other = login(addr, "bob", "phone").await;
    assert_eq!(get_json(addr, "/relay/conversations", "invalid").await.0, 401);
    assert_eq!(get_json(addr, "/relay/conversations", &phone).await.1["enabled"], false);
    assert_eq!(post(addr, "/relay/conversations", Some(&desktop), update()).await.0, 428);
    enable(addr, &phone).await;
    assert_eq!(post(addr, "/relay/conversations", Some(&phone), update()).await.0, 403);
    let mut spoof = update(); spoof["account"] = json!("bob");
    assert_eq!(post(addr, "/relay/conversations", Some(&desktop), spoof).await.0, 400);
    let (status, result) = post(addr, "/relay/conversations", Some(&desktop), update()).await;
    assert_eq!(status, 200, "{result}");
    let entry = &result["conversation"];
    assert_eq!(entry["machine_id"], "archive-desktop");
    let path = format!("/relay/conversations/{}?generation={}&offset=0&end={}", entry["id"].as_str().unwrap(), entry["generation"].as_str().unwrap(), entry["bytes"]);
    assert_eq!(get_json(addr, &path, &phone).await.1["chunk"]["raw"], "first message\n");
    assert_eq!(get_json(addr, &path, &other).await.0, 404);
    assert!(get_json(addr, "/relay/conversations", &other).await.1["conversations"].as_array().unwrap().is_empty());
    assert_eq!(post(addr, "/relay/conversations", Some(&desktop), update()).await.0, 409);
    let restarted = spawn_relay(fixture.gate()).await;
    assert_eq!(get_json(restarted, &path, &phone).await.1["chunk"]["raw"], "first message\n");
    assert_eq!(post(addr, "/relay/logout", Some(&phone), json!({})).await.0, 200);
    assert_eq!(get_json(addr, &path, &phone).await.0, 401);
}

#[tokio::test]
async fn revocation_while_the_upload_body_arrives_cannot_commit_an_archive() {
    let fixture = Fixture::new(); let addr = spawn_relay(fixture.gate()).await;
    let desktop = login(addr, "alice", "desktop").await;
    let phone = login(addr, "alice", "phone").await; enable(addr, &phone).await;
    let body = serde_json::to_vec(&update()).unwrap();
    let (send, receive) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(4);
    let token = desktop.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new().post(format!("http://{addr}/relay/conversations"))
            .bearer_auth(token).header("content-type", "application/json")
            .body(reqwest::Body::wrap_stream(tokio_stream::wrappers::ReceiverStream::new(receive))).send().await.unwrap()
    });
    send.send(Ok(body[..body.len()/2].to_vec())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(post(addr, "/relay/logout", Some(&desktop), json!({})).await.0, 200);
    let _ = send.send(Ok(body[body.len()/2..].to_vec())).await; drop(send);
    assert_eq!(request.await.unwrap().status(), 401);
    assert!(get_json(addr, "/relay/conversations", &phone).await.1["conversations"].as_array().unwrap().is_empty());
}

struct Desktop {
    id: String,
    source: std::sync::Mutex<String>,
    calls: std::sync::Mutex<Vec<u64>>,
}
impl kasa_socket::backend::Backend for Desktop {
    fn list_workspaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::WorkspaceInfo>> { Ok(vec![]) }
    fn current_workspace(&self) -> anyhow::Result<Option<kasa_socket::backend::WorkspaceInfo>> { Ok(None) }
    fn list_surfaces(&self) -> anyhow::Result<Vec<kasa_socket::backend::SurfaceInfo>> { Ok(vec![]) }
    fn focus_surface(&self, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
    fn split_surface(&self, _: kasa_socket::SplitDirection, _: bool, _: Option<&str>) -> anyhow::Result<kasa_socket::backend::SurfaceInfo> { anyhow::bail!("unexpected") }
    fn send_key(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
    fn send_text(&self, _: Option<&str>, _: &str) -> anyhow::Result<()> { anyhow::bail!("unexpected") }
    fn pane_windows(&self) -> Vec<(String, usize)> { vec![(self.id.clone(), 0)] }
    fn collab_pane_identity(&self, _: &str) -> anyhow::Result<Value> { Ok(json!({"surface_key":"key","machine_id":"archive-desktop"})) }
    fn transcript_archive(&self, _: &str, offset: u64) -> anyhow::Result<kasa_socket::backend::TranscriptChunk> {
        self.calls.lock().unwrap().push(offset);
        let source = self.source.lock().unwrap().clone();
        let (raw, next) = match offset { 0 => (format!("first:{source}\n"), 100), 100 => (format!("last:{source}\n"), 200), _ => (String::new(), 200) };
        Ok(kasa_socket::backend::TranscriptChunk { raw, offset: next, source, reset: offset == 0 })
    }
}

#[tokio::test]
async fn desktop_sync_uploads_the_whole_history_and_tracks_source_changes_without_a_phone() {
    let fixture = Fixture::new(); let addr = spawn_relay(fixture.gate()).await;
    let token = login(addr, "alice", "desktop").await;
    let credential = crate::device_auth::DeviceCred { relay:format!("http://{addr}"), account:"alice".into(), device_id:"fixture".into(), token:token.clone(), display_name:None };
    let source = Desktop { id:format!("archive-test-{}", uuid::Uuid::new_v4()), source:std::sync::Mutex::new("first.jsonl".into()), calls:Default::default() };
    let client = reqwest::Client::new();
    let sync = || crate::conversation_archive::sync::synchronize(&source, &client, &credential, "archive-desktop", &|| true);
    assert!(!sync().await.unwrap()); assert!(source.calls.lock().unwrap().is_empty(), "disabled accounts are not read");
    enable(addr, &token).await;
    assert!(sync().await.unwrap()); assert!(sync().await.unwrap()); assert!(!sync().await.unwrap());
    let (_, list) = get_json(addr, "/relay/conversations", &token).await;
    let old = &list["conversations"][0]; assert_eq!(old["ready"], true); assert_eq!(old["cursor"], 200);
    let path = format!("/relay/conversations/{}?generation={}&offset=0&end={}", old["id"].as_str().unwrap(), old["generation"].as_str().unwrap(), old["bytes"]);
    assert_eq!(get_json(addr, &path, &token).await.1["chunk"]["raw"], "first:first.jsonl\nlast:first.jsonl\n");
    *source.source.lock().unwrap() = "second.jsonl".into();
    assert!(sync().await.unwrap()); assert!(sync().await.unwrap()); assert!(!sync().await.unwrap());
    assert_eq!(get_json(addr, "/relay/conversations", &token).await.1["conversations"].as_array().unwrap().len(), 2);
    assert_eq!(get_json(addr, &path, &token).await.1["chunk"]["raw"], "first:first.jsonl\nlast:first.jsonl\n");
}
