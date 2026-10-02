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
            display: String::new(),
        },
        device: Device {
            provider: Provider::Github,
            kind: "desktop".into(),
            machine_id: "machine-one".into(),
        },
        label: "Fixture".into(),
        link,
        choose: false,
        connect: None,
        grant: None,
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
        display: String::new(),
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

#[tokio::test]
async fn redirect_login_skips_code_page_and_redeems_only_with_verifier() {
    let (gate, dir) = fixture();
    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Github,
        subject: "123".into(),
        display: String::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router(gate)).await.unwrap() });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://{address}/relay/oauth");
    let providers: Value = client
        .get(format!("{base}/providers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(providers["redirect_login"], true);
    let verifier = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    let redirect_uri = "kasaterm://oauth";
    let start = |redirect: &str, method: &str| {
        client.post(format!("{base}/start")).json(&json!({
            "provider":"github","kind":"phone","machine_id":"machine-one","label":"Phone",
            "code_challenge":crate::oauth_accounts::pkce_challenge(verifier),
            "code_challenge_method":method,"redirect_uri":redirect,"state":"app-state"
        }))
    };
    let refused = start("https://attacker.example/cb", "S256")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 400);
    assert_eq!(
        refused.json::<Value>().await.unwrap()["error"],
        "invalid_redirect_uri"
    );
    assert_eq!(
        start(redirect_uri, "plain")
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );

    // Browser → provider → app redirect, then redeem; returns the code and the token response.
    let login = || async {
        let started: Value = start(redirect_uri, "S256")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(started.get("user_code").is_none() && started.get("poll_token").is_none());
        let id = started["request_id"].as_str().unwrap();
        assert_eq!(
            started["authorization_url"],
            format!("https://relay.example/relay/oauth/authorize/{id}")
        );
        let browser = client
            .get(format!("{base}/authorize/{id}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            browser.status().as_u16(),
            303,
            "redirect login showed a code page"
        );
        let provider =
            reqwest::Url::parse(browser.headers()["location"].to_str().unwrap()).unwrap();
        assert_eq!(provider.host_str(), Some("github.com"));
        let state = provider
            .query_pairs()
            .find(|(key, _)| key == "state")
            .unwrap()
            .1
            .into_owned();
        let cookie = browser.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let back = client
            .get(format!("{base}/github/callback"))
            .query(&[("state", state.as_str()), ("code", "provider-code")])
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(back.status().as_u16(), 303);
        let back = reqwest::Url::parse(back.headers()["location"].to_str().unwrap()).unwrap();
        assert_eq!(back.scheme(), "kasaterm");
        let pairs: HashMap<_, _> = back.query_pairs().into_owned().collect();
        assert_eq!(pairs["state"], "app-state");
        pairs["code"].clone()
    };
    let redeem = |code: String, verifier: &'static str| {
        client.post(format!("{base}/token")).json(&json!({
            "code":code,"code_verifier":verifier,"redirect_uri":redirect_uri
        }))
    };
    let code = login().await;
    let intercepted = redeem(
        code.clone(),
        "attacker-verifier-0123456789-abcdefghijklmnopqrst",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(intercepted.status().as_u16(), 400);
    assert_eq!(
        redeem(code, verifier)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );

    let code = login().await;
    let done: Value = redeem(code.clone(), verifier)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(done["status"], "complete");
    assert!(done["token"].as_str().unwrap().starts_with("kdt_"));
    let replay: Value = redeem(code, verifier)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay["error"], "invalid_grant");
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

/// First sign-in with an unlinked provider identity asks before creating an account; naming an
/// existing account costs its password once, after which the provider alone signs in.
#[tokio::test]
async fn unlinked_sign_in_asks_then_claims_existing_account_once() {
    let (gate, dir) = fixture();
    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Google,
        subject: "google-sub-1".into(),
        display: "person@example.com".into(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = gate.clone();
    let server = tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://{address}/relay/oauth");
    let providers: Value = client.get(format!("{base}/providers")).send().await.unwrap().json().await.unwrap();
    assert_eq!(providers["choose_account"], true);
    let verifier = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    let redirect_uri = "kasaterm://oauth";
    let login = || async {
        let started: Value = client
            .post(format!("{base}/start"))
            .json(&json!({
                "provider":"google","kind":"phone","machine_id":"machine-one","label":"Phone",
                "code_challenge":crate::oauth_accounts::pkce_challenge(verifier),
                "code_challenge_method":"S256","redirect_uri":redirect_uri,"choose":true
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = started["request_id"].as_str().unwrap();
        let browser = client.get(format!("{base}/authorize/{id}")).send().await.unwrap();
        let provider = reqwest::Url::parse(browser.headers()["location"].to_str().unwrap()).unwrap();
        let state = provider.query_pairs().find(|(key, _)| key == "state").unwrap().1.into_owned();
        let cookie = browser.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        let back = client
            .get(format!("{base}/google/callback"))
            .query(&[("state", state.as_str()), ("code", "provider-code")])
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap();
        let back = reqwest::Url::parse(back.headers()["location"].to_str().unwrap()).unwrap();
        let code = back.query_pairs().find(|(key, _)| key == "code").unwrap().1.into_owned();
        client
            .post(format!("{base}/token"))
            .json(&json!({"code":code,"code_verifier":verifier,"redirect_uri":redirect_uri}))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()
    };
    let post = |path: &str, body: Value| {
        let request = client.post(format!("{base}/{path}")).json(&body);
        async move {
            let response = request.send().await.unwrap();
            (response.status().as_u16(), response.json::<Value>().await.unwrap())
        }
    };

    let held = login().await;
    assert_eq!(held["status"], "choose");
    assert_eq!(held["display"], "person@example.com");
    assert_eq!(held["signup_enabled"], true);
    assert!(held.get("token").is_none(), "an unlinked sign-in got a device credential");
    let ticket = held["ticket"].as_str().unwrap().to_string();
    let wrong = post("claim", json!({"ticket":ticket,"account":"one","password":"wrong"})).await;
    assert_eq!((wrong.0, wrong.1["error"].as_str()), (401, Some("bad_credentials")));
    let forged = post("claim", json!({"ticket":"forged-ticket","account":"one","password":"fixture"})).await;
    assert_eq!(forged.0, 400);
    let claimed = post("claim", json!({"ticket":ticket,"account":"One ","password":"fixture"})).await;
    assert_eq!(claimed.0, 200);
    assert_eq!(claimed.1["status"], "complete");
    assert_eq!(claimed.1["account"], "one");
    assert!(gate.device_by_token(claimed.1["token"].as_str().unwrap()).is_some());
    let replay = post("claim", json!({"ticket":ticket,"account":"one","password":"fixture"})).await;
    assert_eq!(replay.0, 400, "a held sign-in was used twice");

    let again = login().await;
    assert_eq!(again["status"], "complete", "the claimed identity still asked");
    assert_eq!(again["account"], "one");

    // A different identity can start a new account, but only through its own ticket.
    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Google,
        subject: "google-sub-2".into(),
        display: "new@example.com".into(),
    });
    let fresh = login().await;
    assert_eq!(fresh["status"], "choose");
    let ticket = fresh["ticket"].as_str().unwrap().to_string();
    for _ in 0..5 {
        post("claim", json!({"ticket":ticket,"account":"two","password":"guess"})).await;
    }
    let burned = post("signup", json!({"ticket":ticket})).await;
    assert_eq!(burned.0, 400, "a ticket survived repeated wrong passwords");
    let fresh = login().await;
    let created = post("signup", json!({"ticket":fresh["ticket"]})).await;
    assert_eq!(created.0, 200);
    assert!(created.1["account"].as_str().unwrap().starts_with("oauth_"));
    server.abort();
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn held_sign_in_respects_disabled_signup_and_existing_links() {
    let (gate, dir) = fixture();
    let mut ready = ready(None);
    ready.choose = true;
    let held = response_json(complete(&gate, ready.clone())).await;
    assert_eq!(held["status"], "choose");
    let mut oauth = crate::oauth_accounts::OAuth::new(
        Some(dir.join("relay-oauth-identities.json")),
        gate.oauth.config.clone(),
    );
    oauth.config = crate::oauth_accounts::tests::without_signup(oauth.config);
    let ticket = oauth.hold(ready.clone()).unwrap();
    let mut closed = gate.clone();
    closed.oauth = Arc::new(oauth);
    let request = axum::http::Request::builder()
        .body(axum::body::Body::from(json!({"ticket":ticket}).to_string()))
        .unwrap();
    assert_eq!(signup(State(closed.clone()), request).await.status(), StatusCode::FORBIDDEN);
    // Once linked elsewhere, a held ticket cannot move the identity to another account.
    closed.oauth.resolve(&ready.identity, Some("two"), |_| true).unwrap();
    let request = axum::http::Request::builder()
        .body(axum::body::Body::from(
            json!({"ticket":ticket,"account":"one","password":"fixture"}).to_string(),
        ))
        .unwrap();
    assert_eq!(claim(State(closed), request).await.status(), StatusCode::CONFLICT);
    let _ = std::fs::remove_dir_all(dir);
}
