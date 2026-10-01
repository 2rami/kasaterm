use super::*;
use serde_json::{json, Value};

const TOKEN: &str = "Zq3vX9kLm2Pw8RtY4nBc7HdF";

fn gate_in(dir: &std::path::Path, admins: &str) -> Gate {
    std::fs::create_dir_all(dir).unwrap();
    let accounts = dir.join("relay-accounts.json");
    let mut file = crate::relay_auth::AccountsFile::default();
    for name in ["boss", "guest"] {
        file.accounts.insert(
            name.into(),
            crate::relay_auth::Account {
                pbkdf2_sha256: crate::relay_auth::hash_password_with("correct horse", 1000),
                created: 1,
                disabled: false,
            },
        );
    }
    crate::relay_auth::save_accounts(&accounts, &file).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("relay-state.json")), Some(accounts));
    gate.admins = Arc::new(admin::Admins::new(Some(admins)));
    gate
}

fn put_release(dir: &std::path::Path, token: &str, build: &str) {
    let at = dir.join("relay-install").join(token);
    std::fs::create_dir_all(&at).unwrap();
    std::fs::write(at.join(IPA), vec![7u8; CHUNK * 2 + 5]).unwrap();
    std::fs::write(
        at.join("meta.json"),
        json!({"bundle_id":"com.example.app","version":"1.0.0","build":build,"title":"A&B","uploaded":9}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("relay-install/latest"), token).unwrap();
}

async fn serve(gate: Gate) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(gate).into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await
            .unwrap()
    });
    addr
}

async fn login(addr: std::net::SocketAddr, account: &str) -> String {
    let reply: Value = reqwest::Client::new()
        .post(format!("http://{addr}/relay/login"))
        .json(&json!({"account":account,"password":"correct horse","machine_id":format!("{account}-phone"),"kind":"phone"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    reply["token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn serves_the_current_release_by_token_only() {
    let dir = std::env::temp_dir().join(format!("kasa-install-{}", uuid::Uuid::new_v4()));
    let gate = gate_in(&dir, "boss");
    put_release(&dir, TOKEN, "2610011200");
    let addr = serve(gate).await;
    let get = |path: String| async move { reqwest::get(format!("http://{addr}{path}")).await.unwrap() };

    let page = get(format!("/relay/install/{TOKEN}/")).await;
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    let body = page.text().await.unwrap();
    assert!(body.contains("A&amp;B") && body.contains("2610011200"));
    let port = addr.port();
    assert!(body.contains(&format!("itms-services://?action=download-manifest&amp;url=https%3A%2F%2F127.0.0.1%3A{port}%2Frelay%2Finstall%2F{TOKEN}%2Fmanifest.plist")));

    let plist = get(format!("/relay/install/{TOKEN}/manifest.plist")).await.text().await.unwrap();
    assert!(plist.contains(&format!("<string>https://127.0.0.1:{port}/relay/install/{TOKEN}/{IPA}</string>")));
    assert!(plist.contains("<string>com.example.app</string>") && plist.contains("<string>A&amp;B</string>"));

    let ipa = get(format!("/relay/install/{TOKEN}/{IPA}")).await;
    assert_eq!(ipa.status(), 200);
    assert_eq!(ipa.bytes().await.unwrap().len(), CHUNK * 2 + 5);

    for path in [
        "/relay/install/Zq3vX9kLm2Pw8RtY4nBc7HdG/".to_string(),
        "/relay/install/short/manifest.plist".to_string(),
        format!("/relay/install/{TOKEN}/..%2Flatest"),
        "/relay/install/..%2F..%2Frelay-state.json/manifest.plist".to_string(),
    ] {
        assert_eq!(get(path.clone()).await.status(), 404, "{path}");
    }
}

#[tokio::test]
async fn latest_is_for_admin_devices_only() {
    let dir = std::env::temp_dir().join(format!("kasa-install-{}", uuid::Uuid::new_v4()));
    let addr = serve(gate_in(&dir, "boss")).await;
    let boss = login(addr, "boss").await;
    let guest = login(addr, "guest").await;
    let latest = |token: Option<String>| async move {
        let mut req = reqwest::Client::new().get(format!("http://{addr}/relay/install/latest"));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let reply = req.send().await.unwrap();
        (reply.status().as_u16(), reply.json::<Value>().await.unwrap_or_default())
    };

    assert_eq!(latest(None).await.0, 401);
    assert_eq!(latest(Some(guest)).await.0, 403);
    let (status, empty) = latest(Some(boss.clone())).await;
    assert_eq!((status, empty["release"].clone()), (200, Value::Null));

    put_release(&dir, TOKEN, "2610011200");
    let (_, now) = latest(Some(boss)).await;
    assert_eq!(now["release"]["build"], "2610011200");
    assert!(now["release"]["page"].as_str().unwrap().ends_with(&format!("/relay/install/{TOKEN}/")));
    assert!(now["release"]["install"].as_str().unwrap().starts_with("itms-services://?action=download-manifest&url=https%3A%2F%2F"));
}

#[test]
fn reads_device_attributes_out_of_the_signed_envelope() {
    let mut body = vec![0x30, 0x80, 0x06, 0x09, 0xff, 0x00];
    body.extend_from_slice(
        b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\n<key>CHALLENGE</key>\n\t<string>abc</string>\
<key>DEVICE_NAME</key><string>Ann &amp; Bo</string><key>UDID</key><string>00008140-001059A41431801C</string>\
<key>PRODUCT</key><string>iPhone17,1</string><key>LIST</key><array><string>x</string></array></dict></plist>",
    );
    body.extend_from_slice(&[0xa0, 0x82, 0x01]);
    let a = enroll::device_attrs(&body).unwrap();
    assert_eq!(a["CHALLENGE"], "abc");
    assert_eq!(a["DEVICE_NAME"], "Ann & Bo");
    assert_eq!(a["PRODUCT"], "iPhone17,1");
    assert!(enroll::device_attrs(b"no plist here").is_none());

    assert!(enroll::valid_udid("00008140-001059A41431801C"));
    assert!(enroll::valid_udid(&"a1".repeat(20)));
    for bad in ["", "00008140-001059A41431801", "0000814Z-001059A41431801C", "../../etc", &"a".repeat(41)] {
        assert!(!enroll::valid_udid(bad), "{bad}");
    }
}

fn fake_signer(dir: &std::path::Path, reply: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join("signer.sh");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> \"{}\"\ncase \"$1\" in sign-profile) cat ;; register) echo '{reply}' ;; esac\n",
            dir.join("calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

async fn device_post(client: &reqwest::Client, addr: std::net::SocketAddr, challenge: &str, udid: &str) -> reqwest::Response {
    let plist = format!(
        "<plist version=\"1.0\"><dict><key>CHALLENGE</key><string>{challenge}</string><key>UDID</key><string>{udid}</string>\
<key>PRODUCT</key><string>iPhone17,1</string><key>VERSION</key><string>23A</string><key>DEVICE_NAME</key><string>yangona</string></dict></plist>"
    );
    let mut body = vec![0x30, 0x80];
    body.extend_from_slice(plist.as_bytes());
    client.post(format!("http://{addr}/relay/install/{TOKEN}/enroll")).body(body).send().await.unwrap()
}

async fn new_challenge(client: &reqwest::Client, addr: std::net::SocketAddr) -> String {
    let res = client.get(format!("http://{addr}/relay/install/{TOKEN}/enroll.mobileconfig")).send().await.unwrap();
    assert_eq!(res.headers()[header::CONTENT_TYPE], "application/x-apple-aspen-config");
    let text = res.text().await.unwrap();
    assert!(text.contains("<string>Profile Service</string>"));
    assert!(text.contains(&format!("/relay/install/{TOKEN}/enroll</string>")));
    let at = text.find("<key>Challenge</key><string>").unwrap() + 28;
    text[at..at + text[at..].find('<').unwrap()].to_string()
}

#[tokio::test]
async fn a_new_phone_registers_itself_and_comes_back_to_install() {
    let dir = std::env::temp_dir().join(format!("kasa-install-{}", uuid::Uuid::new_v4()));
    let mut gate = gate_in(&dir, "boss");
    gate.enroll = Arc::new(Enroll::new(Some(fake_signer(&dir, r#"{"ok":true,"new":true}"#)), 1));
    put_release(&dir, TOKEN, "2610011200");
    let addr = serve(gate).await;
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();

    let first = client.get(format!("http://{addr}/relay/install/{TOKEN}/")).send().await.unwrap().text().await.unwrap();
    assert!(first.contains("처음이면 — 기기 등록") && first.contains("enroll.mobileconfig"));

    let challenge = new_challenge(&client, addr).await;
    assert_eq!(device_post(&client, addr, &challenge, "not-a-udid").await.status(), 400);
    // challenge 는 꼴이 틀린 요청에도 한 번 쓰면 끝이다.
    assert_eq!(device_post(&client, addr, &challenge, "00008140-001059A41431801C").await.status(), 400);
    assert_eq!(device_post(&client, addr, "made-up", "00008140-001059A41431801C").await.status(), 400);

    let challenge = new_challenge(&client, addr).await;
    let res = device_post(&client, addr, &challenge, "00008140-001059A41431801C").await;
    assert_eq!(res.status(), 301);
    let location = res.headers()[header::LOCATION].to_str().unwrap().to_string();
    let path = &location[location.find("/relay/install/").unwrap()..];
    let mut done = None;
    for _ in 0..50 {
        let res = client.get(format!("http://{addr}{path}")).send().await.unwrap();
        let cookie = res.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_string());
        let body = res.text().await.unwrap();
        if body.contains("기기를 등록했어요") {
            done = Some((body, cookie));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (body, cookie) = done.expect("등록 작업이 끝나지 않았다");
    assert!(body.contains("itms-services://"));
    let cookie = cookie.unwrap();
    assert!(cookie.starts_with("kasa_install_device=1;") && cookie.contains("HttpOnly"));

    let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
    assert!(calls.contains(&format!("register {} 00008140-001059A41431801C yangona", dir.join("relay-install").join(TOKEN).display())));
    let log = std::fs::read_to_string(dir.join("relay-install/registrations.jsonl")).unwrap();
    assert!(log.contains("\"result\":\"registered\"") && log.contains("iPhone17,1"));

    let again = client
        .get(format!("http://{addr}/relay/install/{TOKEN}/"))
        .header(header::COOKIE, "kasa_install_device=1")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(again.contains(">설치</a>") && !again.contains("처음이면"));

    // 하루 한도(1대)를 다 썼다 — 다음 기기는 서명기를 부르지 않는다.
    let challenge = new_challenge(&client, addr).await;
    let res = device_post(&client, addr, &challenge, "00008132-001265E634B9001C").await;
    let location = res.headers()[header::LOCATION].to_str().unwrap().to_string();
    let body = client.get(&location.replacen("https://", "http://", 1)).send().await.unwrap().text().await.unwrap();
    assert!(body.contains("한도"));
    assert_eq!(std::fs::read_to_string(dir.join("calls")).unwrap().matches("register").count(), 1);
}

#[tokio::test]
async fn enrollment_is_hidden_and_closed_without_a_signer_and_limited_per_ip() {
    let dir = std::env::temp_dir().join(format!("kasa-install-{}", uuid::Uuid::new_v4()));
    put_release(&dir, TOKEN, "2610011200");
    let addr = serve(gate_in(&dir, "boss")).await;
    let page = reqwest::get(format!("http://{addr}/relay/install/{TOKEN}/")).await.unwrap().text().await.unwrap();
    assert!(!page.contains("enroll.mobileconfig"));
    assert_eq!(reqwest::get(format!("http://{addr}/relay/install/{TOKEN}/enroll.mobileconfig")).await.unwrap().status(), 404);

    let dir = std::env::temp_dir().join(format!("kasa-install-{}", uuid::Uuid::new_v4()));
    let mut gate = gate_in(&dir, "boss");
    gate.enroll = Arc::new(Enroll::new(Some(fake_signer(&dir, r#"{"ok":true,"new":false}"#)), 10));
    put_release(&dir, TOKEN, "2610011200");
    let addr = serve(gate).await;
    let mut codes = Vec::new();
    for _ in 0..12 {
        codes.push(reqwest::get(format!("http://{addr}/relay/install/{TOKEN}/enroll.mobileconfig")).await.unwrap().status().as_u16());
    }
    assert_eq!(codes.iter().filter(|c| **c == 200).count(), 10);
    assert_eq!(codes.last(), Some(&429));
}
