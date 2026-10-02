use super::*;

pub(crate) fn fixture() -> (OAuth, PathBuf) {
    let dir = std::env::temp_dir().join(format!("kasa-oauth-test-{}", uuid::Uuid::new_v4()));
    let config = Config::from_values(|key| match key {
        "KASA_OAUTH_PUBLIC_ORIGIN" => Some("https://relay.example".into()),
        "KASA_OAUTH_GOOGLE_CLIENT_ID" | "KASA_OAUTH_GITHUB_CLIENT_ID" => Some("fixture-id".into()),
        "KASA_OAUTH_GOOGLE_CLIENT_SECRET" | "KASA_OAUTH_GITHUB_CLIENT_SECRET" => {
            Some("fixture-secret".into())
        }
        "KASA_OAUTH_ALLOW_SIGNUP" => Some("1".into()),
        _ => None,
    });
    (
        OAuth::new(Some(dir.join("relay-oauth-identities.json")), config),
        dir,
    )
}

fn start(oauth: &OAuth) -> (Value, Poll) {
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

#[test]
fn unconfigured_and_malformed_origins_are_disabled() {
    assert!(!Config::default().enabled(Provider::Google));
    for value in [
        "http://relay.example",
        "https://user:secret@relay.example",
        "https://relay.example/callback",
        "https://relay.example/?x=1",
        "https://relay.example/#x",
        "https://relay.example:444",
    ] {
        assert!(!valid_origin(value), "{value}");
    }
    assert!(valid_origin("https://relay.example"));
    let (oauth, dir) = fixture();
    assert!(oauth.storage_ready());
    assert!(
        !oauth
            .config
            .providers(true)
            .to_string()
            .contains("fixture-secret")
    );
    let broken = OAuth::new(None, oauth.config);
    assert_eq!(
        broken.config.providers(broken.storage_ready())["providers"][0]["enabled"],
        false
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn state_cookie_pkce_provider_and_replay_are_bound() {
    let (oauth, dir) = fixture();
    let (started, input) = start(&oauth);
    let browser = oauth.browser(&input.request_id).unwrap();
    let cookie = browser.cookie;
    let state = oauth.pending.lock().unwrap()[&input.request_id]
        .state
        .clone();
    assert!(
        oauth.callback(Provider::Google, &state, &cookie).is_err(),
        "GET did not require explicit confirmation"
    );
    assert!(
        oauth
            .confirm(
                &input.request_id,
                "",
                &browser.csrf,
                started["user_code"].as_str().unwrap()
            )
            .is_err()
    );
    assert!(
        oauth
            .confirm(
                &input.request_id,
                &cookie,
                "wrong",
                started["user_code"].as_str().unwrap()
            )
            .is_err()
    );
    assert!(
        oauth
            .confirm(&input.request_id, &cookie, &browser.csrf, "wrong-code")
            .is_err()
    );
    let url = oauth
        .confirm(
            &input.request_id,
            &cookie,
            &browser.csrf,
            started["user_code"].as_str().unwrap(),
        )
        .unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let query: HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["code_challenge"].len(), 43);
    assert!(query.contains_key("nonce"));
    assert!(!url.to_string().contains(&input.poll_token));
    assert!(oauth.browser(&input.request_id).is_err());
    assert!(
        oauth
            .confirm(
                &input.request_id,
                &cookie,
                &browser.csrf,
                started["user_code"].as_str().unwrap()
            )
            .is_err()
    );
    assert!(oauth.callback(Provider::Google, "wrong", &cookie).is_err());
    assert!(
        oauth
            .callback(Provider::Github, &query["state"], &cookie)
            .is_err()
    );
    assert!(
        oauth
            .callback(Provider::Google, &query["state"], "")
            .is_err()
    );
    let (id, _) = oauth
        .callback(Provider::Google, &query["state"], &cookie)
        .unwrap();
    assert!(
        oauth
            .callback(Provider::Google, &query["state"], &cookie)
            .is_err()
    );
    oauth.finish(
        &id,
        Ok(Identity {
            provider: Provider::Google,
            subject: "123".into(),
            display: String::new(),
        }),
    );
    assert!(oauth.poll(&input, false).unwrap().is_some());
    assert!(oauth.poll(&input, false).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn poll_cannot_cross_device_provider_kind_or_capability() {
    let (oauth, dir) = fixture();
    let (_, mut input) = start(&oauth);
    input.machine_id = "machine-two".into();
    assert!(oauth.poll(&input, false).is_err());
    input.machine_id = "machine-one".into();
    input.provider = Provider::Github;
    assert!(oauth.poll(&input, false).is_err());
    input.provider = Provider::Google;
    input.kind = "phone".into();
    assert!(oauth.poll(&input, false).is_err());
    input.kind = "desktop".into();
    let correct = input.poll_token.clone();
    input.poll_token = "wrong".into();
    assert!(oauth.poll(&input, false).is_err());
    input.poll_token = correct;
    assert!(oauth.poll(&input, false).unwrap().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn expiration_cancel_and_failed_exchange_are_terminal() {
    let (oauth, dir) = fixture();
    let (_, input) = start(&oauth);
    oauth
        .pending
        .lock()
        .unwrap()
        .get_mut(&input.request_id)
        .unwrap()
        .created = Instant::now() - TTL;
    assert!(oauth.browser(&input.request_id).is_err());
    assert!(oauth.poll(&input, false).is_err());
    let (_, input) = start(&oauth);
    assert!(oauth.poll(&input, true).unwrap().is_none());
    oauth.finish(
        &input.request_id,
        Ok(Identity {
            provider: Provider::Google,
            subject: "123".into(),
            display: String::new(),
        }),
    );
    assert!(oauth.poll(&input, false).is_err());
    let (started, input) = start(&oauth);
    let browser = oauth.browser(&input.request_id).unwrap();
    let cookie = browser.cookie;
    let url = oauth
        .confirm(
            &input.request_id,
            &cookie,
            &browser.csrf,
            started["user_code"].as_str().unwrap(),
        )
        .unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let state = url.query_pairs().find(|(key, _)| key == "state").unwrap().1;
    let (id, _) = oauth.callback(Provider::Google, &state, &cookie).unwrap();
    oauth.finish(&id, Err("cancelled"));
    assert_eq!(oauth.poll(&input, false).err(), Some("cancelled"));
    assert_eq!(oauth.poll(&input, false).err(), Some("expired"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn device_confirmation_codes_are_flow_specific_and_attempt_limited() {
    let (oauth, dir) = fixture();
    let (_, one) = start(&oauth);
    let (other, _) = start(&oauth);
    let browser = oauth.browser(&one.request_id).unwrap();
    assert_eq!(other["user_code"].as_str().unwrap().len(), 9);
    for _ in 0..5 {
        assert!(
            oauth
                .confirm(
                    &one.request_id,
                    &browser.cookie,
                    &browser.csrf,
                    other["user_code"].as_str().unwrap()
                )
                .is_err()
        );
    }
    assert!(oauth.browser(&one.request_id).is_err());
    assert_eq!(oauth.poll(&one, false).err(), Some("invalid_confirmation"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn identities_are_namespaced_never_email_merged_and_conflicts_fail() {
    let (oauth, dir) = fixture();
    let google = Identity {
        provider: Provider::Google,
        subject: "123".into(),
        display: String::new(),
    };
    let github = Identity {
        provider: Provider::Github,
        subject: "123".into(),
        display: String::new(),
    };
    assert_eq!(
        oauth
            .resolve(&google, Some("existing-one"), |_| false)
            .unwrap(),
        "existing-one"
    );
    assert_eq!(
        oauth.resolve(&google, None, |_| false).unwrap(),
        "existing-one"
    );
    assert_eq!(
        oauth
            .resolve(&google, Some("existing-two"), |_| false)
            .err(),
        Some("already_linked")
    );
    let second = oauth.resolve(&github, None, |_| false).unwrap();
    assert_ne!(second, "existing-one");
    assert!(oauth.active(&second));
    let reloaded = OAuth::new(oauth.path.clone(), oauth.config.clone());
    assert_eq!(reloaded.resolve(&github, None, |_| false).unwrap(), second);
    assert!(!reloaded.active("existing-one"));
    let raw = std::fs::read_to_string(oauth.path.as_ref().unwrap()).unwrap();
    assert!(!raw.contains("fixture-secret"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(oauth.path.as_ref().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn signup_is_opt_in_and_corrupt_storage_is_not_replaced() {
    let (mut oauth, dir) = fixture();
    oauth.config.allow_signup = false;
    let identity = Identity {
        provider: Provider::Google,
        subject: "123".into(),
        display: String::new(),
    };
    assert_eq!(
        oauth.resolve(&identity, None, |_| false).err(),
        Some("account_not_linked")
    );
    assert_eq!(
        oauth
            .resolve(&identity, Some("existing"), |_| false)
            .unwrap(),
        "existing"
    );
    let path = oauth.path.clone().unwrap();
    std::fs::write(&path, "invalid original").unwrap();
    let broken = OAuth::new(Some(path.clone()), oauth.config);
    assert!(!broken.storage_ready());
    assert!(
        broken
            .resolve(&identity, Some("another"), |_| false)
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "invalid original");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn google_claims_require_verified_email_issuer_audience_nonce_and_time() {
    let good = json!({"iss":"https://accounts.google.com","aud":"client","sub":"123","nonce":"nonce","iat":100,"exp":200,"email_verified":true});
    assert_eq!(
        google_claims(&good, "client", "nonce", 150)
            .unwrap()
            .subject,
        "123"
    );
    for (key, value) in [
        ("iss", json!("https://evil.example")),
        ("aud", json!("other")),
        ("nonce", json!("wrong")),
        ("exp", json!(150)),
        ("iat", json!(999)),
        ("email_verified", json!(false)),
        ("sub", json!("")),
        ("azp", json!("other")),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        assert!(
            google_claims(&bad, "client", "nonce", 150).is_err(),
            "{key}"
        );
    }
    assert!(
        google_identity(
            "unsigned.payload.signature",
            &json!({"keys":[]}),
            "client",
            "nonce",
            150
        )
        .is_err()
    );
    let token = format!(
        "{}.{}.eA",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(good.to_string())
    );
    assert!(google_identity(&token, &json!({"keys":[]}), "client", "nonce", 150).is_err());
}

#[test]
fn github_requires_stable_numeric_id_not_username_or_email() {
    assert!(github_identity(&json!({"login":"a","email":"a@example.com"})).is_err());
    assert!(github_identity(&json!({"id":"123"})).is_err());
    assert_eq!(
        github_identity(&json!({"id":123,"login":"changed"}))
            .unwrap()
            .subject,
        "123"
    );
}

#[test]
fn google_rs256_signature_accepts_local_fixture_and_rejects_tampering() {
    let fixture: Value = serde_json::from_str(include_str!("signed-fixture.json")).unwrap();
    let token = fixture["token"].as_str().unwrap();
    assert_eq!(
        google_identity(token, &fixture["jwks"], "fixture", "fixture-nonce", 150)
            .unwrap()
            .subject,
        "123"
    );
    let mut parts: Vec<String> = token.split('.').map(str::to_string).collect();
    parts[1] = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
    assert!(
        google_identity(
            &parts.join("."),
            &fixture["jwks"],
            "fixture",
            "fixture-nonce",
            150
        )
        .is_err()
    );
    assert!(google_identity(token, &fixture["jwks"], "other-app", "fixture-nonce", 150).is_err());
    assert!(google_identity(token, &fixture["jwks"], "fixture", "other-nonce", 150).is_err());
    assert!(google_identity(token, &fixture["jwks"], "fixture", "fixture-nonce", 200).is_err());
}

const VERIFIER: &str = "fixture-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
const LOOPBACK: &str = "http://127.0.0.1:53682/oauth/callback";

fn native_start(oauth: &OAuth) -> String {
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
        )
        .unwrap();
    assert!(value.get("user_code").is_none() && value.get("poll_token").is_none());
    value["request_id"].as_str().unwrap().into()
}

/// Walks a redirect flow through the browser and provider; returns the code sent to the app.
fn native_code(oauth: &OAuth, id: &str) -> String {
    let (cookie, url) = oauth.redirect(id).unwrap().unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    assert_eq!(url.host_str(), Some("accounts.google.com"));
    let state = url.query_pairs().find(|(key, _)| key == "state").unwrap().1;
    let (request, _) = oauth.callback(Provider::Google, &state, &cookie).unwrap();
    let back = oauth
        .finish(
            &request,
            Ok(Identity {
                provider: Provider::Google,
                subject: "123".into(),
                display: String::new(),
            }),
        )
        .unwrap();
    assert!(back.starts_with(&format!("{LOOPBACK}?")));
    let back = reqwest::Url::parse(&back).unwrap();
    let pairs: HashMap<_, _> = back.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], "app-state");
    pairs["code"].clone()
}

fn redeem(
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

#[test]
fn redirect_flow_needs_no_typed_code_but_only_the_verifier_redeems() {
    let (oauth, dir) = fixture();
    let id = native_start(&oauth);
    assert!(
        oauth.browser(&id).is_err(),
        "redirect flow showed a code page"
    );
    assert!(oauth.confirm(&id, "", "", "").is_err());
    let code = native_code(&oauth, &id);
    assert!(
        oauth.redirect(&id).is_err(),
        "browser re-entered after callback"
    );
    let other = "other-verifier-0123456789-abcdefghijklmnopqrstuvwxyz";
    assert_eq!(
        redeem(&oauth, &code, other, LOOPBACK).err(),
        Some("invalid_grant")
    );
    assert_eq!(
        redeem(&oauth, &code, VERIFIER, LOOPBACK).err(),
        Some("invalid_grant"),
        "intercepted code stayed usable after a failed attempt"
    );

    let id = native_start(&oauth);
    let code = native_code(&oauth, &id);
    assert!(redeem(&oauth, &code, VERIFIER, "http://127.0.0.1:1/other").is_err());

    let id = native_start(&oauth);
    let code = native_code(&oauth, &id);
    let ready = redeem(&oauth, &code, VERIFIER, LOOPBACK).unwrap();
    assert_eq!(ready.identity.subject, "123");
    assert_eq!(ready.device.machine_id, "machine-one");
    assert_eq!(
        redeem(&oauth, &code, VERIFIER, LOOPBACK).err(),
        Some("invalid_grant")
    );
    assert!(redeem(&oauth, "", VERIFIER, LOOPBACK).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn redirect_flow_cannot_be_polled_and_codes_expire() {
    let (oauth, dir) = fixture();
    let id = native_start(&oauth);
    let code = native_code(&oauth, &id);
    let poll = Poll {
        request_id: id.clone(),
        poll_token: String::new(),
        provider: Provider::Google,
        kind: "desktop".into(),
        machine_id: "machine-one".into(),
    };
    assert!(
        oauth.poll(&poll, false).is_err(),
        "poll released a redirect result"
    );
    oauth
        .pending
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .code
        .as_mut()
        .unwrap()
        .1 = Instant::now() - CODE_TTL;
    assert_eq!(
        redeem(&oauth, &code, VERIFIER, LOOPBACK).err(),
        Some("invalid_grant")
    );

    let id = native_start(&oauth);
    oauth.pending.lock().unwrap().get_mut(&id).unwrap().created = Instant::now() - TTL;
    assert_eq!(oauth.redirect(&id).err(), Some("expired"));

    let id = native_start(&oauth);
    let (cookie, url) = oauth.redirect(&id).unwrap().unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let state = url.query_pairs().find(|(key, _)| key == "state").unwrap().1;
    let (request, _) = oauth.callback(Provider::Google, &state, &cookie).unwrap();
    let back = oauth.finish(&request, Err("cancelled")).unwrap();
    assert_eq!(
        back,
        format!("{LOOPBACK}?error=access_denied&state=app-state")
    );
    assert!(!oauth.pending.lock().unwrap().contains_key(&id));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_loopback_ip_and_first_party_app_redirects_are_accepted() {
    for ok in [
        "http://127.0.0.1:53682/oauth/callback",
        "http://[::1]:8080/cb",
        "kasaterm://oauth",
        "kasaterm:/oauth",
        "nachochat://oauth/callback",
    ] {
        assert!(valid_redirect_uri(ok), "{ok}");
    }
    for bad in [
        "http://localhost:53682/cb",
        "https://127.0.0.1:53682/cb",
        "http://127.0.0.1/cb",
        "http://127.0.0.2:5000/cb",
        "http://10.0.0.5:5000/cb",
        "https://attacker.example/cb",
        "http://127.0.0.1:5000/cb?next=https://attacker.example",
        "http://127.0.0.1:5000/cb#frag",
        "http://user@127.0.0.1:5000/cb",
        "javascript:alert(1)",
        "file:///etc/passwd",
        "evilapp://oauth",
        "",
    ] {
        assert!(!valid_redirect_uri(bad), "{bad}");
    }
    assert!(valid_challenge(&pkce_challenge(VERIFIER)));
    assert!(!valid_challenge("short"));
    assert!(!valid_verifier("too-short"));
    assert!(!valid_client_state("bad state"));
}

pub(crate) fn without_signup(mut config: Config) -> Config {
    config.allow_signup = false;
    config
}
