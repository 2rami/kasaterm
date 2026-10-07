use super::*;
use crate::connections::{Feature, Grant};
use crate::oauth_accounts::{Identity, Provider};
use serde_json::Value;

async fn rig() -> (String, reqwest::Client, String, crate::connections::tests::Seen, Gate, PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let config = oauth.config.clone();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    accounts.accounts.insert(
        "one".into(),
        crate::relay_auth::Account {
            pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture", 1),
            disabled: false,
            created: 1,
            login: None,
        },
    );
    crate::relay_auth::save_accounts(&dir.join("accounts.json"), &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(dir.join("accounts.json")));
    gate.oauth = Arc::new(oauth);
    // The gateway opened its own store with the environment's (empty) provider setup; release its
    // single-writer lock and open the store again with the fixture's providers.
    gate.connections = None;
    let mut service = crate::connections::Service::open(Some(&dir.join("state.json")), config).unwrap();
    let (base, calls) = crate::connections::tests::providers().await;
    Arc::get_mut(&mut service).unwrap().endpoints = crate::connections::tests::endpoints(&base);
    gate.connections = Some(service);
    let token = crate::relay_auth::new_token();
    gate.devices.lock().unwrap().insert(
        "dev_desktop_one".into(),
        DeviceRec {
            token_hash: crate::relay_auth::token_hash(&token),
            account: "one".into(),
            kind: "desktop".into(),
            machine_id: Some("machine-one".into()),
            label: "Laptop".into(),
            created: 1,
            last_seen: 1,
            revoked_at: None,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let served = gate.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    (format!("http://{address}/relay"), client, token, calls, gate, dir)
}

const VERIFIER: &str = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
const REDIRECT: &str = "http://127.0.0.1:53682/oauth/callback";

fn start_body(provider: &str, connect: Value, native: bool) -> Value {
    let mut body = json!({"provider":provider,"kind":"desktop","machine_id":"machine-one","label":"Laptop",
        "link":true,"connect":connect});
    if native {
        body["code_challenge"] = json!(crate::oauth_accounts::pkce_challenge(VERIFIER));
        body["code_challenge_method"] = json!("S256");
        body["redirect_uri"] = json!(REDIRECT);
    }
    body
}

/// Runs a redirect connect flow through the provider callback and redeems it with the verifier.
/// Returns the provider authorization query and the token response.
async fn round_trip(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    body: Value,
) -> (std::collections::HashMap<String, String>, Value) {
    let provider_name = body["provider"].as_str().unwrap().to_string();
    let started: Value = client
        .post(format!("{base}/oauth/start"))
        .bearer_auth(token)
        .json(&body)
        .send().await.unwrap().json().await.unwrap();
    let id = started["request_id"].as_str().unwrap();
    let browser = client.get(format!("{base}/oauth/authorize/{id}")).send().await.unwrap();
    assert_eq!(browser.status(), 303);
    let provider = reqwest::Url::parse(browser.headers()["location"].to_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = provider.query_pairs().into_owned().collect();
    let cookie = browser.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
    let back = client
        .get(format!("{base}/oauth/{provider_name}/callback"))
        .query(&[("state", query["state"].as_str()), ("code", "provider-code")])
        .header("cookie", &cookie)
        .send().await.unwrap();
    let back = reqwest::Url::parse(back.headers()["location"].to_str().unwrap()).unwrap();
    let code = back.query_pairs().find(|(key, _)| key == "code").unwrap().1.into_owned();
    let connected: Value = client
        .post(format!("{base}/oauth/token"))
        .json(&json!({"code":code,"code_verifier":VERIFIER,"redirect_uri":REDIRECT}))
        .send().await.unwrap().json().await.unwrap();
    (query, connected)
}

#[tokio::test]
async fn connect_flow_stores_tokens_only_for_the_asking_account_and_writes_need_approval() {
    let (base, client, token, calls, gate, dir) = rig().await;
    assert_eq!(client.get(format!("{base}/connections")).send().await.unwrap().status(), 401);

    // A connect flow must come from a signed-in device, prove its redirect, and ask for its own provider.
    let start = |body: Value, bearer: bool| {
        let request = client.post(format!("{base}/oauth/start")).json(&body);
        if bearer { request.bearer_auth(&token) } else { request }
    };
    let mut unlinked = start_body("github", json!(["github.pr"]), true);
    unlinked["link"] = json!(false);
    for (body, bearer) in [
        (unlinked, false),
        (start_body("github", json!(["github.pr"]), false), true),
        (start_body("google", json!(["github.pr"]), true), true),
    ] {
        assert_eq!(start(body, bearer).send().await.unwrap().status(), 400);
    }

    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Github,
        subject: "1001".into(),
        display: "octo".into(),
        picture: None,
    });
    *gate.oauth.mock_grant.lock().unwrap() = Some(Grant {
        provider: Provider::Github,
        subject: "1001".into(),
        display: "octo".into(),
        access_token: "gh-access-1".into(),
        access_expires: crate::relay_auth::now_secs() + 28800,
        refresh_token: Some("gh-refresh-1".into()),
        refresh_expires: 0,
    });
    let (_, connected) = round_trip(&client, &base, &token, start_body("github", json!(["github.pr"]), true)).await;
    assert_eq!(connected["status"], "connected");
    assert_eq!(connected["connection"]["features"], json!(["github.pr"]));
    assert!(connected.get("token").is_none(), "a connect flow handed out a device credential");

    let listed: Value = client.get(format!("{base}/connections")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 1);
    assert_eq!(listed["available"], json!({"google":false,"github":true}));
    assert_eq!(listed["github_install_url"], "https://github.com/apps/kasa-work/installations/new");
    assert!(!listed.to_string().contains("gh-access"));

    let queued: Value = client
        .post(format!("{base}/connections/pr/create"))
        .bearer_auth(&token)
        .json(&json!({"repo":"2rami/kasaterm","base":"main","head":"feat/x","title":"t"}))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(queued["status"], "pending");
    let (pending, digest) = (queued["pending"]["id"].as_str().unwrap(), queued["pending"]["digest"].as_str().unwrap());
    let approve = |key: Option<&str>| {
        let request = client
            .post(format!("{base}/connections/pending/{pending}/approve"))
            .bearer_auth(&token)
            .json(&json!({"digest":digest}));
        match key { Some(key) => request.header("x-kasa-approver", key), None => request }
    };
    let refused = approve(None).send().await.unwrap();
    assert_eq!(refused.status(), 403);
    assert_eq!(refused.json::<Value>().await.unwrap()["error"], "approver_required");
    let key = crate::oauth_accounts::secret();
    assert_eq!(client.post(format!("{base}/connections/approver")).bearer_auth(&token).json(&json!({"key":key})).send().await.unwrap().status(), 200);
    let created: Value = approve(Some(&key)).send().await.unwrap().json().await.unwrap();
    assert_eq!(created["status"], "created");
    assert_eq!(crate::connections::tests::seen(&calls, "github/pulls").len(), 1);

    let audit: Value = client.get(format!("{base}/connections/audit")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(audit["audit"].as_array().unwrap().iter().any(|line| line["action"] == "pr.create" && line["result"] == "ok"));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn google_links_sign_in_only_and_mail_is_gone() {
    let (base, client, token, _, gate, dir) = rig().await;
    let providers: Value = client.get(format!("{base}/oauth/providers")).send().await.unwrap().json().await.unwrap();
    assert_eq!(providers["connect"], json!({"google":false,"github":true}));
    // An older app still asking for Gmail is refused before any consent screen opens.
    for features in [json!(["mail.read", "mail.send"]), json!(["mail.read"])] {
        let refused = client
            .post(format!("{base}/oauth/start"))
            .bearer_auth(&token)
            .json(&start_body("google", features, true))
            .send().await.unwrap();
        assert_eq!(refused.status(), 400);
    }
    for path in ["mail/list", "mail/read", "mail/send"] {
        let gone = client.post(format!("{base}/connections/{path}")).bearer_auth(&token).json(&json!({})).send().await.unwrap();
        assert!(matches!(gone.status().as_u16(), 404 | 405), "{path} still answers");
    }

    // Linking Google from settings asks for identity scopes and nothing else.
    *gate.oauth.mock_identity.lock().unwrap() = Some(Identity {
        provider: Provider::Google,
        subject: "google-sub".into(),
        display: "me@example.com".into(),
        picture: None,
    });
    let (query, linked) = round_trip(&client, &base, &token, start_body("google", json!([]), true)).await;
    assert_eq!(query["scope"], "openid email profile");
    assert!(!query.contains_key("access_type") && !query.contains_key("include_granted_scopes"));
    assert_eq!(linked["status"], "linked");
    let listed: Value = client.get(format!("{base}/connections")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(listed["connections"].as_array().unwrap().is_empty(), "a Google link kept provider tokens");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn github_connect_uses_the_app_without_scopes() {
    let (base, client, token, _, _, dir) = rig().await;
    let started: Value = client
        .post(format!("{base}/oauth/start"))
        .bearer_auth(&token)
        .json(&start_body("github", json!(["github.pr"]), true))
        .send().await.unwrap().json().await.unwrap();
    let id = started["request_id"].as_str().unwrap();
    let browser = client.get(format!("{base}/oauth/authorize/{id}")).send().await.unwrap();
    let provider = reqwest::Url::parse(browser.headers()["location"].to_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = provider.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "app-id");
    assert!(!query.contains_key("scope"));
    assert!(matches!(Feature::GithubPr.provider(), Provider::Github));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn github_connect_links_the_login_and_reports_a_missing_install() {
    let (base, client, token, calls, gate, dir) = rig().await;
    let github = |subject: &str| Identity { provider: Provider::Github, subject: subject.into(), display: "octo".into(), picture: None };
    let grant = |subject: &str| Grant {
        provider: Provider::Github,
        subject: subject.into(),
        display: "octo".into(),
        access_token: "gh-access-1".into(),
        access_expires: crate::relay_auth::now_secs() + 28800,
        refresh_token: Some("gh-refresh-1".into()),
        refresh_expires: crate::relay_auth::now_secs() + 15897600,
    };
    *gate.oauth.mock_identity.lock().unwrap() = Some(github("1001"));
    *gate.oauth.mock_grant.lock().unwrap() = Some(grant("1001"));
    let (_, connected) = round_trip(&client, &base, &token, start_body("github", json!(["github.pr"]), true)).await;
    assert_eq!(connected["status"], "connected");
    assert_eq!(connected["linked"], true);
    assert_eq!(connected["installed"], false);
    assert_eq!(connected["install_url"], "https://github.com/apps/kasa-work/installations/new");
    assert_eq!(crate::connections::tests::seen(&calls, "github/installations"), vec!["Bearer gh-access-1"]);
    assert_eq!(gate.oauth.lookup(&github("1001")).as_deref(), Some("one"));

    // A GitHub that already signs in to another account keeps that sign-in; the work tokens still land here.
    gate.oauth.resolve(&github("2002"), Some("other"), |_| true).unwrap();
    *gate.oauth.mock_identity.lock().unwrap() = Some(github("2002"));
    *gate.oauth.mock_grant.lock().unwrap() = Some(grant("2002"));
    let (_, connected) = round_trip(&client, &base, &token, start_body("github", json!(["github.pr"]), true)).await;
    assert_eq!(connected["status"], "connected");
    assert_eq!(connected["linked"], false);
    assert_eq!(connected["link_error"], "already_linked");
    assert_eq!(gate.oauth.lookup(&github("2002")).as_deref(), Some("other"));
    let listed: Value = client.get(format!("{base}/connections")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(listed["connections"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(dir);
}
