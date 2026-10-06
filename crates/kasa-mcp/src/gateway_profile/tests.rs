use super::*;
use crate::oauth_accounts::Identity;
use serde_json::Value;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfixture-pixels";

async fn rig() -> (String, reqwest::Client, String, String, Gate, PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    for name in ["one", "two"] {
        accounts.accounts.insert(
            name.into(),
            crate::relay_auth::Account {
                pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture-pass", 1),
                disabled: false,
                created: 1,
                login: None,
            },
        );
    }
    crate::relay_auth::save_accounts(&dir.join("accounts.json"), &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(dir.join("accounts.json")));
    gate.oauth = Arc::new(oauth);
    let signup = Identity { provider: Provider::Google, subject: "g-new".into(), display: "new@example.com".into(), picture: None };
    let created = gate.oauth.resolve(&signup, None, |_| false).unwrap();
    let mut tokens = Vec::new();
    for (id, account) in [("dev_one", "one"), ("dev_oauth", created.as_str())] {
        let token = crate::relay_auth::new_token();
        gate.devices.lock().unwrap().insert(
            id.into(),
            DeviceRec {
                token_hash: crate::relay_auth::token_hash(&token),
                account: account.into(),
                kind: "desktop".into(),
                machine_id: Some(format!("machine-{id}")),
                label: "Laptop".into(),
                created: 1,
                last_seen: 1,
                revoked_at: None,
            },
        );
        tokens.push(token);
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let served = gate.clone();
    tokio::spawn(async move { axum::serve(listener, router(served).into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let oauth_token = tokens.pop().unwrap();
    (format!("http://{address}/relay"), reqwest::Client::new(), tokens.pop().unwrap(), oauth_token, gate, dir)
}

async fn get(client: &reqwest::Client, url: String, token: &str) -> (u16, Value) {
    let response = client.get(url).bearer_auth(token).send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap_or(Value::Null))
}

async fn post(client: &reqwest::Client, url: String, token: &str, body: Value) -> (u16, Value) {
    let response = client.post(url).bearer_auth(token).json(&body).send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn profile_names_linked_logins_and_prefers_the_uploaded_picture() {
    let (base, client, token, _, gate, dir) = rig().await;
    assert_eq!(client.get(format!("{base}/profile")).send().await.unwrap().status(), 401);
    let link = |provider, subject: &str, display: &str, picture: Option<&str>| {
        gate.oauth
            .resolve(
                &Identity {
                    provider,
                    subject: subject.into(),
                    display: display.into(),
                    picture: picture.map(str::to_string),
                },
                Some("one"),
                |name| gate.accounts.exists(name),
            )
            .unwrap();
    };
    link(Provider::Github, "4242", "octo", None);
    link(Provider::Google, "g-1", "me@example.com", Some("https://lh3.googleusercontent.com/a/face"));
    let (status, profile) = get(&client, format!("{base}/profile"), &token).await;
    assert_eq!(status, 200);
    assert_eq!(profile["login"], "one");
    assert_eq!(profile["has_password"], true);
    assert_eq!(profile["identities"][0]["provider"], "google", "Google comes first");
    assert_eq!(profile["identities"][0]["display"], "me@example.com");
    assert_eq!(profile["identities"][1]["display"], "octo");
    assert_eq!(profile["identities"][1]["picture"], "https://avatars.githubusercontent.com/u/4242?v=4");
    assert_eq!(profile["avatar"]["source"], "google");

    let patched: Value = client.patch(format!("{base}/profile")).bearer_auth(&token)
        .json(&json!({"nickname":"  건호\u{7} ","avatar":"github"})).send().await.unwrap().json().await.unwrap();
    assert_eq!(patched["nickname"], "건호");
    assert_eq!(patched["display_name"], "건호");
    assert_eq!(patched["avatar"]["source"], "github");
    assert_eq!(get(&client, format!("{base}/whoami"), &token).await.1["display_name"], "건호");
    let too_long = "가".repeat(MAX_NICKNAME + 1);
    let refused = client.patch(format!("{base}/profile")).bearer_auth(&token)
        .json(&json!({"nickname": too_long})).send().await.unwrap();
    assert_eq!(refused.status(), 400);

    let not_image = client.put(format!("{base}/profile/avatar")).bearer_auth(&token)
        .body("<svg/>").send().await.unwrap();
    assert_eq!(not_image.status(), 415);
    let uploaded: Value = client.put(format!("{base}/profile/avatar")).bearer_auth(&token)
        .body(PNG).send().await.unwrap().json().await.unwrap();
    assert_eq!(uploaded["avatar"]["source"], "upload");
    assert!(uploaded["avatar"]["rev"].as_str().is_some_and(|rev| rev.len() == 16));
    let image = client.get(format!("{base}/profile/avatar")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(image.headers()["content-type"], "image/png");
    assert_eq!(image.bytes().await.unwrap().as_ref(), PNG);
    let stored = std::fs::read_dir(dir.join("relay-avatars")).unwrap().next().unwrap().unwrap();
    assert!(!stored.file_name().to_string_lossy().contains("one"), "the account name became a file name");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(stored.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    let removed: Value = client.delete(format!("{base}/profile/avatar")).bearer_auth(&token)
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(removed["avatar"]["source"], "google", "the provider picture comes back after removing the upload");
    assert_eq!(client.get(format!("{base}/profile/avatar")).bearer_auth(&token).send().await.unwrap().status(), 404);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn login_and_password_change_need_the_current_password() {
    let (base, client, token, oauth_token, _gate, dir) = rig().await;
    let url = format!("{base}/profile/login");
    assert_eq!(post(&client, url.clone(), &token, json!({"password":"wrong","login":"kasa"})).await.0, 401);
    assert_eq!(post(&client, url.clone(), &token, json!({"password":"fixture-pass","login":"two"})).await, (409, json!({"ok":false,"error":"login_taken"})));
    assert_eq!(post(&client, url.clone(), &token, json!({"password":"fixture-pass","login":"Bad Name"})).await.0, 400);
    assert_eq!(post(&client, url.clone(), &oauth_token, json!({"password":"x","login":"kasa"})).await.1["error"], "no_password");
    let (status, profile) = post(&client, url.clone(), &token, json!({"password":"fixture-pass","login":" Kasa "})).await;
    assert_eq!(status, 200);
    assert_eq!(profile["login"], "kasa");

    let login = |name: &str, password: &str| {
        client.post(format!("{base}/login")).json(&json!({"account":name,"password":password,"kind":"phone","label":"Phone"})).send()
    };
    let signed = login("kasa", "fixture-pass").await.unwrap();
    assert_eq!(signed.status(), 200);
    assert_eq!(signed.json::<Value>().await.unwrap()["account"], "one", "the account key stays the same");
    assert_eq!(login("one", "fixture-pass").await.unwrap().status(), 401, "the old login still signed in");

    let url = format!("{base}/profile/password");
    assert_eq!(post(&client, url.clone(), &token, json!({"password":"fixture-pass","new_password":"short"})).await.1["error"], "weak_password");
    assert_eq!(post(&client, url.clone(), &token, json!({"password":"fixture-pass","new_password":"brand-new-pass"})).await.0, 200);
    assert_eq!(login("kasa", "fixture-pass").await.unwrap().status(), 401);
    assert_eq!(login("kasa", "brand-new-pass").await.unwrap().status(), 200);
    assert_eq!(get(&client, format!("{base}/whoami"), &token).await.0, 200, "changing the password signed this device out");
    let _ = std::fs::remove_dir_all(&dir);
}
