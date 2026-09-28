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
    };
    let github = Identity {
        provider: Provider::Github,
        subject: "123".into(),
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
