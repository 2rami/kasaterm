use super::*;
use super::super::tests::{account_gate, get_json, post, spawn_relay};
use serde_json::Value;

async fn login(addr: std::net::SocketAddr, account: &str, kind: &str, machine: &str) -> String {
    let (status, body) = post(addr, "/relay/login", None, json!({
        "account": account, "password": "correct horse", "kind": kind, "machine_id": machine, "label": machine,
    }))
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().unwrap().to_string()
}

async fn put_state(addr: std::net::SocketAddr, token: &str, body: Value) -> u16 {
    reqwest::Client::new()
        .put(format!("http://{addr}/relay/launcher/state"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

fn state(name: &str) -> Value {
    json!({"name": name, "os": "macos", "apps": [
        {"id": "kasadesk", "name": "KasaDesk", "installed": "0.1.9", "latest": "0.1.14", "status": "update_available"},
    ]})
}

fn rig() -> (Gate, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("kasa-launcher-{}", uuid::Uuid::new_v4()));
    let gate = account_gate(&dir);
    let accounts_path = dir.join("relay-accounts.json");
    let mut accounts = kasa_identity::relay_auth::load_accounts(&accounts_path);
    let other = accounts.accounts["tester"].clone();
    accounts.accounts.insert("other".into(), other);
    kasa_identity::relay_auth::save_accounts(&accounts_path, &accounts).unwrap();
    (gate, dir)
}

#[tokio::test]
async fn launcher_token_reaches_only_launcher_routes() {
    let (gate, dir) = rig();
    let addr = spawn_relay(gate).await;
    let launcher = login(addr, "tester", "launcher", "launcher-mac-1").await;
    let desktop = login(addr, "tester", "desktop", "launcher-mac-1").await;

    assert_eq!(get_json(addr, "/relay/whoami", &launcher).await.1["kind"], "launcher");
    for path in ["/relay/account-sync", "/relay/devices", "/relay/profile", "/relay/account/term/panes", "/relay/agent-chains"] {
        assert_eq!(get_json(addr, path, &launcher).await.0, 401, "{path}");
    }
    assert_eq!(get_json(addr, "/relay/launcher/sync", &desktop).await.0, 403);
    assert_eq!(put_state(addr, &desktop, state("맥북")).await, 403);

    // 같은 기계에서 런처로 다시 로그인해도 카사텀 토큰은 그대로다.
    let again = login(addr, "tester", "launcher", "launcher-mac-1").await;
    assert_eq!(get_json(addr, "/relay/whoami", &launcher).await.0, 401);
    assert_eq!(get_json(addr, "/relay/whoami", &again).await.0, 200);
    assert_eq!(get_json(addr, "/relay/whoami", &desktop).await.0, 200);
    assert_eq!(post(addr, "/relay/logout", Some(&again), json!({})).await.0, 200);
    assert_eq!(get_json(addr, "/relay/launcher/sync", &again).await.0, 401);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn install_request_wakes_the_waiting_launcher_of_the_same_account_only() {
    let (gate, dir) = rig();
    let addr = spawn_relay(gate).await;
    let book = login(addr, "tester", "launcher", "launcher-book").await;
    let desk = login(addr, "tester", "launcher", "launcher-desk").await;
    let stranger = login(addr, "other", "launcher", "launcher-else").await;
    assert_eq!(put_state(addr, &book, state("맥북")).await, 200);
    assert_eq!(put_state(addr, &desk, state("데스크탑")).await, 200);
    assert_eq!(put_state(addr, &stranger, state("남의 기기")).await, 200);
    assert_eq!(put_state(addr, &book, json!({"name": "x", "os": "macos", "apps": [{"id": "../etc"}]})).await, 400);

    let (_, seen) = get_json(addr, "/relay/launcher/sync", &book).await;
    let names: Vec<&str> = seen["devices"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["데스크탑", "맥북"]);
    let desk_id = seen["devices"][0]["device_id"].as_str().unwrap().to_string();
    assert_eq!(seen["devices"][0]["apps"][0]["latest"], "0.1.14");
    assert_eq!(seen["devices"][1]["current"], true);

    let (_, mine) = get_json(addr, "/relay/launcher/sync", &desk).await;
    let rev = mine["rev"].as_u64().unwrap();
    let waiting = tokio::spawn({
        let desk = desk.clone();
        async move {
            let started = Instant::now();
            let reply = get_json(addr, &format!("/relay/launcher/sync?rev={rev}&wait=20"), &desk).await;
            (reply, started.elapsed())
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, stranger_view) = get_json(addr, "/relay/launcher/sync", &stranger).await;
    let foreign = stranger_view["devices"][0]["device_id"].as_str().unwrap();
    let path = |id: &str| format!("/relay/launcher/devices/{id}/install");
    assert_eq!(post(addr, &path(foreign), Some(&book), json!({})).await.0, 404);
    assert_eq!(post(addr, &path(&desk_id), Some(&book), json!({"app": "Bad App"})).await.0, 400);
    assert_eq!(post(addr, &path(&desk_id), Some(&book), json!({"url": "https://example.com/x.msi"})).await.0, 400);
    assert_eq!(post(addr, &path(&desk_id), Some(&book), json!({})).await.0, 200);

    let ((status, woke), took) = waiting.await.unwrap();
    assert_eq!(status, 200);
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(woke["commands"].as_array().unwrap().len(), 1);
    assert_eq!(woke["commands"][0]["app"], Value::Null);
    assert_eq!(woke["commands"][0]["from"], "맥북");
    // 가져간 명령은 다시 오지 않는다.
    assert_eq!(get_json(addr, "/relay/launcher/sync", &desk).await.1["commands"], json!([]));
    assert_eq!(get_json(addr, "/relay/launcher/sync", &stranger).await.1["devices"].as_array().unwrap().len(), 1);

    assert_eq!(post(addr, "/relay/logout", Some(&desk), json!({})).await.0, 200);
    let (_, after) = get_json(addr, "/relay/launcher/sync", &book).await;
    assert_eq!(after["devices"].as_array().unwrap().len(), 1);
    assert_eq!(post(addr, &path(&desk_id), Some(&book), json!({})).await.0, 404);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn state_change_ends_the_wait_of_other_launchers() {
    let (gate, dir) = rig();
    let addr = spawn_relay(gate).await;
    let book = login(addr, "tester", "launcher", "launcher-book").await;
    let desk = login(addr, "tester", "launcher", "launcher-desk").await;
    assert_eq!(put_state(addr, &desk, state("데스크탑")).await, 200);
    let (_, first) = get_json(addr, "/relay/launcher/sync", &book).await;
    let rev = first["rev"].as_u64().unwrap();

    let started = Instant::now();
    let (_, quiet) = get_json(addr, &format!("/relay/launcher/sync?rev={rev}&wait=1"), &book).await;
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(quiet["rev"].as_u64().unwrap(), rev);

    let waiting = tokio::spawn({
        let book = book.clone();
        async move { get_json(addr, &format!("/relay/launcher/sync?rev={rev}&wait=20"), &book).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut progress = state("데스크탑");
    progress["apps"][0]["phase"] = "downloading".into();
    progress["apps"][0]["progress"] = 0.4.into();
    assert_eq!(put_state(addr, &desk, progress).await, 200);
    let (_, woke) = tokio::time::timeout(Duration::from_secs(5), waiting).await.unwrap().unwrap();
    assert!(woke["rev"].as_u64().unwrap() > rev);
    assert_eq!(woke["devices"][0]["apps"][0]["phase"], "downloading");
    let _ = std::fs::remove_dir_all(&dir);
}
