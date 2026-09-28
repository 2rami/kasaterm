use super::*;
use crate::oauth_accounts::{Identity, OAuth};
use serde_json::Value;

fn fixture() -> (Gate, std::path::PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    for account in ["one", "two"] {
        accounts.accounts.insert(
            account.into(),
            crate::relay_auth::Account {
                pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture", 1),
                disabled: false,
                created: 1,
            },
        );
    }
    let path = dir.join("accounts.json");
    crate::relay_auth::save_accounts(&path, &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(path));
    gate.oauth = Arc::new(oauth);
    (gate, dir)
}

fn ready(link: Option<Link>) -> Ready {
    Ready {
        identity: Identity {
            provider: Provider::Github,
            subject: "123".into(),
        },
        device: Device {
            provider: Provider::Github,
            kind: "desktop".into(),
            machine_id: "machine-one".into(),
        },
        label: "Fixture".into(),
        link,
    }
}

fn linked_device(gate: &Gate, account: &str, id: &str) -> Link {
    gate.devices.lock().unwrap().insert(
        id.into(),
        DeviceRec {
            token_hash: "fixture-hash".into(),
            account: account.into(),
            kind: "desktop".into(),
            machine_id: Some("machine-one".into()),
            label: "Fixture".into(),
            created: 1,
            last_seen: 1,
            revoked_at: None,
        },
    );
    Link {
        device_id: id.into(),
        account: account.into(),
        token_hash: "fixture-hash".into(),
    }
}

#[test]
fn linking_rechecks_revocation_and_prevents_cross_account_takeover() {
    let (gate, dir) = fixture();
    let one = linked_device(&gate, "one", "one-device");
    let two = linked_device(&gate, "two", "two-device");
    assert_eq!(
        complete(&gate, ready(Some(one.clone()))).status(),
        StatusCode::OK
    );
    assert_eq!(
        complete(&gate, ready(Some(two))).status(),
        StatusCode::CONFLICT
    );
    gate.revoke(&one.device_id);
    assert_eq!(
        complete(&gate, ready(Some(one))).status(),
        StatusCode::UNAUTHORIZED
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn mocked_browser_flow_only_returns_device_token_to_bound_poll() {
    let (gate, dir) = fixture();
    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Github,
        subject: "123".into(),
    });
    let request = axum::http::Request::builder().body(axum::body::Body::from(json!({
        "provider":"github","kind":"desktop","machine_id":"machine-one","label":"Test laptop"
    }).to_string())).unwrap();
    let response = start(State(gate.clone()), request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    let browser = gate
        .oauth
        .browser(response["request_id"].as_str().unwrap())
        .unwrap();
    let cookie = browser.cookie;
    let authorize_url = gate
        .oauth
        .confirm(
            response["request_id"].as_str().unwrap(),
            &cookie,
            &browser.csrf,
            response["user_code"].as_str().unwrap(),
        )
        .unwrap();
    let parsed = reqwest::Url::parse(&authorize_url).unwrap();
    let state = parsed
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(header::COOKIE, cookie.parse().unwrap());
    let response_browser = callback(
        State(gate.clone()),
        AxPath(Provider::Github),
        Query(Callback {
            state,
            code: Some("mock-code".into()),
            error: None,
        }),
        headers,
    )
    .await;
    assert_eq!(response_browser.status(), StatusCode::OK);
    let text = String::from_utf8(
        axum::body::to_bytes(response_browser.into_body(), 8192)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!text.contains("kdt_"));
    let input = json!({"request_id":response["request_id"],"poll_token":response["poll_token"],
        "provider":"github","kind":"desktop","machine_id":"machine-one"});
    let request = || {
        axum::http::Request::builder()
            .body(axum::body::Body::from(input.to_string()))
            .unwrap()
    };
    let result = poll(State(gate.clone()), request()).await;
    assert_eq!(result.status(), StatusCode::OK);
    let result: Value = serde_json::from_slice(
        &axum::body::to_bytes(result.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "complete");
    assert!(
        gate.device_by_token(result["token"].as_str().unwrap())
            .is_some()
    );
    assert_eq!(
        poll(State(gate.clone()), request()).await.status(),
        StatusCode::BAD_REQUEST
    );
    let reloaded = Gate::with_accounts(gate.state_path.clone(), Some(dir.join("accounts.json")));
    assert!(
        reloaded
            .device_by_token(result["token"].as_str().unwrap())
            .is_some()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn missing_config_and_forged_link_do_not_start() {
    let (mut gate, dir) = fixture();
    let request = || {
        axum::http::Request::builder().body(axum::body::Body::from(json!({
        "provider":"github","kind":"desktop","machine_id":"machine-one","label":"Fixture","link":true
    }).to_string())).unwrap()
    };
    assert_eq!(
        start(State(gate.clone()), request()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    gate.oauth = Arc::new(OAuth::new(Some(dir.join("other.json")), Default::default()));
    let response = providers(State(gate)).await;
    let value: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["providers"][0]["enabled"], false);
    assert_eq!(value["providers"][1]["enabled"], false);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn link_confirmation_names_destination_without_html_injection() {
    let page = device_confirmation(
        "fixture",
        &crate::oauth_accounts::BrowserConfirmation {
            cookie: "secret-cookie".into(),
            csrf: "csrf".into(),
            label: "<script>bad</script>".into(),
            machine_id: "machine-one".into(),
            account: Some("one".into()),
            provider: Provider::Github,
        },
    );
    assert!(page.contains("<strong>one</strong>"));
    assert!(page.contains("&lt;script&gt;bad&lt;/script&gt;"));
    assert!(!page.contains("<script>"));
    assert!(page.contains("action=\"/relay/oauth/authorize/fixture\""));
    assert!(page.contains("method=\"post\""));
    assert!(!page.contains("secret-cookie"));
}

#[tokio::test]
async fn oauth_http_routes_apply_no_store_and_never_redirect_to_request_input() {
    let (gate, dir) = fixture();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router(gate)).await.unwrap() });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let result = client
        .get(format!("http://{address}/relay/oauth/providers"))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status().as_u16(), 200);
    assert_eq!(result.headers()["cache-control"], "no-store");
    assert_eq!(result.headers()["referrer-policy"], "no-referrer");
    let started: Value = client
        .post(format!("http://{address}/relay/oauth/start"))
        .json(&json!({
            "provider":"github","kind":"desktop","machine_id":"machine-one","label":"Fixture laptop"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = started["request_id"].as_str().unwrap();
    let result = client
        .get(format!("http://{address}/relay/oauth/authorize/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        result.status().as_u16(),
        200,
        "ordinary login bypassed device confirmation"
    );
    assert!(result.headers().get("location").is_none());
    assert_eq!(
        result.headers()["referrer-policy"],
        "same-origin",
        "confirmation form must send its real Origin"
    );
    let cookie = result.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let page = result.text().await.unwrap();
    assert!(page.contains("Fixture laptop"));
    assert!(
        !page.contains(started["user_code"].as_str().unwrap()),
        "browser disclosed the app-only verification code"
    );
    let csrf = page
        .split("name=\"csrf\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let form = [
        ("csrf", csrf),
        ("user_code", started["user_code"].as_str().unwrap()),
    ];
    let url = format!("http://{address}/relay/oauth/authorize/{id}");
    assert_eq!(
        client
            .post(&url)
            .form(&form)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    assert_eq!(
        client
            .post(&url)
            .header("origin", "https://relay.example")
            .form(&form)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(
        client
            .post(&url)
            .header("origin", "https://relay.example")
            .header("cookie", &cookie)
            .form(&[("csrf", "wrong"), ("user_code", form[1].1)])
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    assert_eq!(
        client
            .post(&url)
            .header("origin", "https://relay.example")
            .header("cookie", &cookie)
            .form(&[("csrf", csrf), ("user_code", "wrong")])
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    let result = client
        .post(&url)
        .header("origin", "https://relay.example")
        .header("cookie", &cookie)
        .form(&form)
        .send()
        .await
        .unwrap();
    assert_eq!(result.status().as_u16(), 303);
    assert!(
        result.headers()["location"]
            .to_str()
            .unwrap()
            .starts_with("https://github.com/login/oauth/authorize?")
    );
    assert_eq!(
        client
            .post(&url)
            .header("origin", "https://relay.example")
            .header("cookie", &cookie)
            .form(&form)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    let result = client.post(format!("http://{address}/relay/oauth/start")).json(&json!({
        "provider":"github","kind":"desktop","machine_id":"machine-one","label":"fixture","redirect_uri":"https://attacker.example"
    })).send().await.unwrap();
    assert_eq!(result.status().as_u16(), 400);
    assert!(result.headers().get("location").is_none());
    let result = client
        .get(format!(
            "http://{address}/relay/oauth/github/callback?state=wrong&code=never-exchanged"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status().as_u16(), 400);
    assert_eq!(result.headers()["cache-control"], "no-store");
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

async fn response_json(response: axum::response::Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn password_device(gate: &Gate) -> Value {
    let request = axum::http::Request::builder().body(axum::body::Body::from(json!({
        "account":"one","password":"fixture","kind":"desktop","machine_id":"machine-one","label":"Current laptop"
    }).to_string())).unwrap();
    response_json(super::super::login(State(gate.clone()), request).await).await
}

#[tokio::test]
async fn late_oauth_poll_never_revokes_a_newer_password_login() {
    let (gate, dir) = fixture();
    gate.oauth
        .resolve(&ready(None).identity, Some("one"), |_| false)
        .unwrap();
    let old_oauth = ready(None);
    let newer_password = password_device(&gate).await;
    let old_completion = response_json(complete(&gate, old_oauth)).await;
    assert!(
        gate.device_by_token(newer_password["token"].as_str().unwrap())
            .is_some()
    );
    gate.revoke(old_completion["device_id"].as_str().unwrap());
    assert!(
        gate.device_by_token(newer_password["token"].as_str().unwrap())
            .is_some(),
        "late client rejection also killed the newer login"
    );
    let reloaded = Gate::with_accounts(gate.state_path.clone(), Some(dir.join("accounts.json")));
    assert!(
        reloaded
            .device_by_token(newer_password["token"].as_str().unwrap())
            .is_some()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn lost_poll_response_and_client_save_failure_keep_existing_login() {
    let (gate, dir) = fixture();
    gate.oauth
        .resolve(&ready(None).identity, Some("one"), |_| false)
        .unwrap();
    let existing = password_device(&gate).await;
    let response_lost = complete(&gate, ready(None));
    drop(response_lost);
    assert!(
        gate.device_by_token(existing["token"].as_str().unwrap())
            .is_some()
    );
    let unsaved = response_json(complete(&gate, ready(None))).await;
    let bad_parent = dir.join("not-a-directory");
    std::fs::write(&bad_parent, "fixture").unwrap();
    assert!(
        crate::relay_auth::write_private(&bad_parent.join("device.json"), &unsaved.to_string())
            .is_err()
    );
    gate.revoke(unsaved["device_id"].as_str().unwrap());
    assert!(
        gate.device_by_token(existing["token"].as_str().unwrap())
            .is_some()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn server_save_failure_does_not_revoke_existing_login() {
    let (mut gate, dir) = fixture();
    gate.oauth
        .resolve(&ready(None).identity, Some("one"), |_| false)
        .unwrap();
    let existing = password_device(&gate).await;
    let bad_state = dir.join("state-is-directory");
    std::fs::create_dir(&bad_state).unwrap();
    gate.state_path = Some(bad_state);
    assert_eq!(
        complete(&gate, ready(None)).status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(
        gate.device_by_token(existing["token"].as_str().unwrap())
            .is_some()
    );
    assert_eq!(
        gate.devices
            .lock()
            .unwrap()
            .values()
            .filter(|device| device.revoked_at.is_none())
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(dir);
}
