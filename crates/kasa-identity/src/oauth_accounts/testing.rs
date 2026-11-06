use super::*;

pub fn fixture() -> (OAuth, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kasa-oauth-test-{}", uuid::Uuid::new_v4()));
    let config = Config::from_values(|key| match key {
        "KASA_OAUTH_PUBLIC_ORIGIN" => Some("https://relay.example".into()),
        "KASA_OAUTH_GOOGLE_CLIENT_ID" | "KASA_OAUTH_GITHUB_CLIENT_ID" => Some("fixture-id".into()),
        "KASA_OAUTH_GOOGLE_CLIENT_SECRET" | "KASA_OAUTH_GITHUB_CLIENT_SECRET" => {
            Some("fixture-secret".into())
        }
        "KASA_OAUTH_ALLOW_SIGNUP" => Some("1".into()),
        "KASA_GITHUB_APP_CLIENT_ID" => Some("app-id".into()),
        "KASA_GITHUB_APP_CLIENT_SECRET" => Some("app-secret".into()),
        "KASA_GITHUB_APP_SLUG" => Some("kasa-work".into()),
        _ => None,
    });
    (
        OAuth::new(Some(dir.join("relay-oauth-identities.json")), config),
        dir,
    )
}

pub fn start(oauth: &OAuth) -> (Value, Poll) {
    let value = oauth
        .start(
            Device {
                provider: Provider::Google,
                kind: "desktop".into(),
                machine_id: "machine-one".into(),
            },
            "Laptop".into(),
            None,
            None,
            false,
            None,
        )
        .unwrap();
    let input = Poll {
        request_id: value["request_id"].as_str().unwrap().into(),
        poll_token: value["poll_token"].as_str().unwrap().into(),
        provider: Provider::Google,
        kind: "desktop".into(),
        machine_id: "machine-one".into(),
    };
    (value, input)
}

pub const VERIFIER: &str = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";

pub const LOOPBACK: &str = "http://127.0.0.1:53682/oauth/callback";

pub fn native_start(oauth: &OAuth) -> String {
    let value = oauth
        .start(
            Device {
                provider: Provider::Google,
                kind: "desktop".into(),
                machine_id: "machine-one".into(),
            },
            "Laptop".into(),
            None,
            Some(Native {
                challenge: pkce_challenge(VERIFIER),
                redirect_uri: LOOPBACK.into(),
                state: Some("app-state".into()),
            }),
            false,
            None,
        )
        .unwrap();
    assert!(value.get("user_code").is_none() && value.get("poll_token").is_none());
    value["request_id"].as_str().unwrap().into()
}

/// Walks a redirect flow through the browser and provider; returns the code sent to the app.
pub fn native_code(oauth: &OAuth, id: &str) -> String {
    let (cookie, url) = oauth.redirect(id).unwrap().unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    let state = url.query_pairs().find(|(key, _)| key == "state").unwrap().1;
    let (request, _) = oauth.callback(Provider::Google, &state, &cookie).unwrap();
    let back = oauth
        .finish(
            &request,
            Ok((Identity {
                provider: Provider::Google,
                subject: "123".into(),
                display: String::new(),
                picture: None,
            }, None)),
        )
        .unwrap();
    assert!(back.starts_with(&format!("{LOOPBACK}?")));
    let back = reqwest::Url::parse(&back).unwrap();
    let pairs: HashMap<_, _> = back.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], "app-state");
    pairs["code"].clone()
}

pub fn redeem(
    oauth: &OAuth,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<Ready, &'static str> {
    oauth.token(&Token {
        code: code.into(),
        code_verifier: verifier.into(),
        redirect_uri: redirect_uri.into(),
    })
}

pub fn without_signup(mut config: Config) -> Config {
    config.allow_signup = false;
    config
}
