use super::*;
use serde_json::{json, Value};

const ORIGIN: &str = "https://relay.example";

fn fixture(admins: Option<&str>) -> (Gate, std::path::PathBuf) {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    std::fs::create_dir_all(&dir).unwrap();
    let mut accounts = crate::relay_auth::AccountsFile::default();
    for account in ["one", "two"] {
        accounts.accounts.insert(
            account.into(),
            crate::relay_auth::Account {
                pbkdf2_sha256: crate::relay_auth::hash_password_with("fixture", 1),
                disabled: false,
                created: 1_700_000_000,
                login: None,
            },
        );
    }
    let path = dir.join("accounts.json");
    crate::relay_auth::save_accounts(&path, &accounts).unwrap();
    let mut gate = Gate::with_accounts(Some(dir.join("state.json")), Some(path));
    gate.oauth = Arc::new(oauth);
    gate.admins = Arc::new(Admins::new(admins));
    (gate, dir)
}

fn identity(provider: Provider, subject: &str, display: &str) -> Identity {
    Identity { provider, subject: subject.into(), display: display.into(), picture: None }
}

async fn serve(gate: Gate) -> (std::net::SocketAddr, reqwest::Client) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(gate)).await.unwrap() });
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    (address, client)
}

fn first_cookie(response: &reqwest::Response) -> String {
    response.headers()[header::SET_COOKIE].to_str().unwrap().split(';').next().unwrap().to_string()
}

fn state_of(location: &str) -> String {
    reqwest::Url::parse(location).unwrap().query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned()
}

async fn callback(
    gate: &Gate,
    address: std::net::SocketAddr,
    client: &reqwest::Client,
    provider: Provider,
    state: &str,
    cookie: &str,
    who: Identity,
) -> reqwest::Response {
    *gate.oauth.mock_identity.lock().unwrap() = Some(who);
    client
        .get(format!("http://{address}/relay/oauth/{}/callback?state={state}&code=fixture-code", provider.name()))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap()
}

/// 앱의 OAuth 가입을 처음부터 끝까지 — 시작·브라우저 확인·제공자 콜백·폴링.
async fn sign_up(gate: &Gate, address: std::net::SocketAddr, client: &reqwest::Client, who: Identity, machine: &str) -> Value {
    let provider = who.provider;
    let started: Value = client
        .post(format!("http://{address}/relay/oauth/start"))
        .json(&json!({"provider":provider.name(),"kind":"desktop","machine_id":machine,"label":"Fixture"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = started["request_id"].as_str().unwrap();
    let page = client.get(format!("http://{address}/relay/oauth/authorize/{id}")).send().await.unwrap();
    let cookie = first_cookie(&page);
    let html = page.text().await.unwrap();
    let csrf = html.split("name=\"csrf\" value=\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let confirmed = client
        .post(format!("http://{address}/relay/oauth/authorize/{id}"))
        .header(header::ORIGIN, ORIGIN)
        .header(header::COOKIE, &cookie)
        .form(&[("csrf", csrf.as_str()), ("user_code", started["user_code"].as_str().unwrap())])
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status().as_u16(), 303);
    let state = state_of(confirmed.headers()[header::LOCATION].to_str().unwrap());
    assert_eq!(callback(gate, address, client, provider, &state, &cookie, who).await.status().as_u16(), 200);
    client
        .post(format!("http://{address}/relay/oauth/poll"))
        .json(&json!({"request_id":id,"poll_token":started["poll_token"],"provider":provider.name(),"kind":"desktop","machine_id":machine}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// 관리 화면 로그인 — 성공이면 관리자 쿠키를, 아니면 콜백 응답 그대로.
async fn admin_login(gate: &Gate, address: std::net::SocketAddr, client: &reqwest::Client, who: Identity) -> reqwest::Response {
    let provider = who.provider;
    let started = client
        .post(format!("http://{address}/relay/admin/login/{}", provider.name()))
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .send()
        .await
        .unwrap();
    assert_eq!(started.status().as_u16(), 303);
    let location = started.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(location.starts_with(match provider {
        Provider::Google => "https://accounts.google.com/",
        Provider::Github => "https://github.com/",
    }));
    assert!(location.contains("code_challenge_method=S256"));
    callback(gate, address, client, provider, &state_of(&location), &first_cookie(&started), who).await
}

#[tokio::test]
async fn only_server_listed_accounts_see_signups_and_no_content_is_exposed() {
    let (gate, dir) = fixture(Some("one"));
    gate.oauth.resolve(&identity(Provider::Github, "900", "teacher-gh"), Some("one"), |_| true).unwrap();
    let (address, client) = serve(gate.clone()).await;

    let alice = sign_up(&gate, address, &client, identity(Provider::Google, "111", "alice@example.com"), "machine-alice").await;
    let bob = sign_up(&gate, address, &client, identity(Provider::Github, "222", "bob-gh"), "machine-bob").await;
    assert_eq!(alice["status"], "complete");
    assert_eq!(alice["display_name"], "alice@example.com");
    assert_eq!(bob["display_name"], "bob-gh");
    let alice_account = alice["account"].as_str().unwrap().to_string();
    assert!(alice_account.starts_with("oauth_"));
    let whoami: Value = client
        .get(format!("http://{address}/relay/whoami"))
        .bearer_auth(alice["token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(whoami["display_name"], "alice@example.com");

    // 관리자 로그인 없이는 목록이 없다.
    let anonymous = client.get(format!("http://{address}/relay/admin")).send().await.unwrap();
    assert_eq!(anonymous.status().as_u16(), 200);
    assert_eq!(anonymous.headers()["cache-control"], "no-store");
    let page = anonymous.text().await.unwrap();
    assert!(page.contains("/relay/admin/login/google") && !page.contains("alice@example.com"));
    assert_eq!(client.get(format!("http://{address}/relay/admin/accounts")).send().await.unwrap().status().as_u16(), 401);

    // 쿠키를 지어내도 관리자가 되지 않는다.
    let forged = client
        .get(format!("http://{address}/relay/admin/accounts"))
        .header(header::COOKIE, format!("{COOKIE}=kdt_forged-value; admin=1; account=one"))
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status().as_u16(), 401);

    // 관리자가 아닌 가입자·처음 보는 신원은 같은 답으로 막히고, 계정도 기기도 안 생긴다.
    let before = (gate.oauth.overview().len(), gate.devices.lock().unwrap().len());
    for who in [identity(Provider::Google, "111", "alice@example.com"), identity(Provider::Google, "333", "stranger@example.com")] {
        let denied = admin_login(&gate, address, &client, who).await;
        assert_eq!(denied.status().as_u16(), 403);
        assert!(denied.headers().get(header::SET_COOKIE).is_none());
    }
    assert_eq!(before, (gate.oauth.overview().len(), gate.devices.lock().unwrap().len()));

    let signed_in = admin_login(&gate, address, &client, identity(Provider::Github, "900", "teacher-gh")).await;
    assert_eq!(signed_in.status().as_u16(), 303);
    assert_eq!(signed_in.headers()[header::LOCATION], "/relay/admin");
    let set_cookie = signed_in.headers()[header::SET_COOKIE].to_str().unwrap().to_string();
    for part in ["Path=/relay/admin", "Secure", "HttpOnly", "SameSite=Lax"] {
        assert!(set_cookie.contains(part), "{set_cookie}");
    }
    let cookie = set_cookie.split(';').next().unwrap().to_string();
    assert_eq!(before.1, gate.devices.lock().unwrap().len(), "admin sign-in issued a device credential");

    let listed: Value = client
        .get(format!("http://{address}/relay/admin/accounts"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = listed["accounts"].as_array().unwrap();
    let row = |account: &str| rows.iter().find(|r| r["account"] == account).unwrap().clone();
    let alice_row = row(&alice_account);
    assert_eq!(alice_row["signup"], "google");
    assert_eq!(alice_row["display_name"], "alice@example.com");
    assert_eq!(alice_row["desktops"], 1);
    assert!(alice_row["created"].as_u64().unwrap() > 1_700_000_000);
    assert_eq!(row(bob["account"].as_str().unwrap())["signup"], "github");
    assert_eq!(row("one")["signup"], "password");
    assert_eq!(row("one")["login_methods"], json!(["password", "github"]));
    let raw = listed.to_string();
    for secret in [alice["token"].as_str().unwrap(), bob["token"].as_str().unwrap(), "token_hash", "pbkdf2", "\"111\"", "\"222\""] {
        assert!(!raw.contains(secret), "admin list exposed {secret}");
    }

    let page = client.get(format!("http://{address}/relay/admin")).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(page.headers()["referrer-policy"], "same-origin");
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("frame-ancestors 'none'"));
    let page = page.text().await.unwrap();
    assert!(page.contains("alice@example.com") && page.contains("bob-gh") && page.contains("관문 가입자"));

    // 다른 곳에서 보낸 로그아웃·로그인 폼은 받지 않는다.
    let cross = client
        .post(format!("http://{address}/relay/admin/logout"))
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status().as_u16(), 403);
    let cross = client
        .post(format!("http://{address}/relay/admin/login/github"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .send()
        .await
        .unwrap();
    assert_eq!(cross.status().as_u16(), 403);

    // 관리자 계정이 막히면 쥔 쿠키도 그 순간 무효.
    let mut accounts = crate::relay_auth::load_accounts(&dir.join("accounts.json"));
    accounts.accounts.get_mut("one").unwrap().disabled = true;
    crate::relay_auth::save_accounts(&dir.join("accounts.json"), &accounts).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    let _ = std::fs::File::options().write(true).open(dir.join("accounts.json")).and_then(|f| f.set_modified(later));
    let blocked = client.get(format!("http://{address}/relay/admin/accounts")).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(blocked.status().as_u16(), 401);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn logout_ends_the_session_and_unconfigured_relays_hide_the_page() {
    let (gate, dir) = fixture(None);
    let (address, client) = serve(gate.clone()).await;
    for path in ["/relay/admin", "/relay/admin/accounts"] {
        assert_eq!(client.get(format!("http://{address}{path}")).send().await.unwrap().status().as_u16(), 404);
    }
    let _ = std::fs::remove_dir_all(dir);

    let (gate, dir) = fixture(Some("one, not valid!, two"));
    assert!(gate.admins.accounts.contains("one") && gate.admins.accounts.contains("two"));
    assert_eq!(gate.admins.accounts.len(), 2);
    gate.oauth.resolve(&identity(Provider::Google, "900", ""), Some("two"), |_| true).unwrap();
    let (address, client) = serve(gate.clone()).await;
    let signed_in = admin_login(&gate, address, &client, identity(Provider::Google, "900", "")).await;
    let cookie = first_cookie(&signed_in);
    let ok = client.get(format!("http://{address}/relay/admin/accounts")).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let out = client
        .post(format!("http://{address}/relay/admin/logout"))
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(out.status().as_u16(), 303);
    assert!(out.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    let after = client.get(format!("http://{address}/relay/admin/accounts")).header(header::COOKIE, &cookie).send().await.unwrap();
    assert_eq!(after.status().as_u16(), 401);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn uplink_usage_is_folded_per_account_and_survives_restart() {
    let dir = std::env::temp_dir().join(format!("kasa-usage-{}", uuid::Uuid::new_v4()));
    let path = dir.join("relay-usage.json");
    let mut gate = Gate::with_accounts(None, None);
    gate.usage = Arc::new(Meter::open(Some(path.clone())));
    let (tx, _rx) = mpsc::channel(1);
    let up = Uplink {
        conn: 1,
        machine: "fixture".into(),
        machine_id: "fixture-machine".into(),
        aliases: Vec::new(),
        last_seen: Mutex::new(Instant::now()),
        tx,
        streams: Mutex::new(HashMap::new()),
        next: AtomicU32::new(1),
        account: Some("alice".into()),
        device_id: Some("dev_fixture".into()),
        owner_slug: None,
        nacho_app: AtomicBool::new(false),
        kick: tokio::sync::Notify::new(),
        rx_bytes: AtomicU64::new(1000),
        tx_bytes: AtomicU64::new(24),
        requests: AtomicU64::new(3),
        metered: Mutex::new(Instant::now() - Duration::from_secs(90)),
    };
    gate.meter(&up);
    gate.meter(&up);
    let usage = gate.usage.snapshot()["alice"].clone();
    assert_eq!((usage.relayed_bytes, usage.requests), (1024, 3), "counters must be folded once");
    assert!((90_000..95_000).contains(&usage.connected_ms));
    gate.usage.save();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(Meter::open(Some(path.clone())).snapshot()["alice"], usage);
    std::fs::write(&path, "not json").unwrap();
    assert!(Meter::open(Some(path.clone())).snapshot().is_empty());
    assert!(!path.exists(), "unreadable usage must be moved aside, not overwritten");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn profile_keeps_signup_time_and_follows_the_signup_provider_name() {
    let (oauth, dir) = crate::oauth_accounts::tests::fixture();
    let google = identity(Provider::Google, "111", "old@example.com");
    let account = oauth.resolve(&google, None, |_| false).unwrap();
    let created = oauth.overview()[&account].profile.clone().unwrap().created;
    oauth.resolve(&identity(Provider::Github, "5", "linked-gh"), Some(&account), |_| true).unwrap();
    oauth.resolve(&identity(Provider::Github, "5", "linked-gh"), None, |_| true).unwrap();
    assert_eq!(oauth.display_name(&account).as_deref(), Some("old@example.com"), "a linked provider renamed the account");
    oauth.resolve(&identity(Provider::Google, "111", "new@example.com"), None, |_| true).unwrap();
    assert_eq!(oauth.display_name(&account).as_deref(), Some("new@example.com"));
    let info = &oauth.overview()[&account];
    assert_eq!(info.profile.as_ref().unwrap().created, created);
    assert_eq!(info.providers, vec![Provider::Github, Provider::Google]);
    assert_eq!(oauth.lookup(&identity(Provider::Google, "999", "")), None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn page_text_is_escaped_and_times_are_korean_standard_time() {
    assert_eq!(kst(0), "—");
    assert_eq!(kst(1_759_276_800), "2025-10-01 09:00");
    assert_eq!(bytes(512), "512 B");
    assert_eq!(bytes(3 * 1024 * 1024), "3.0 MB");
    assert_eq!(duration(59), "1분");
    let row = Row {
        account: "oauth_x".into(),
        display_name: Some("<script>alert(1)</script>".into()),
        signup: "github",
        created: 0,
        created_estimated: false,
        login_methods: vec!["github"],
        active: true,
        desktops: 0,
        phones: 0,
        online: 0,
        revoked: 0,
        last_seen: 0,
        connected_secs: 0,
        relayed_bytes: 0,
        requests: 0,
    };
    let html = dashboard("one\"><b>", &[row]);
    assert!(!html.contains("<script>") && html.contains("&lt;script&gt;") && !html.contains("one\"><b>"));
}

/// 검증 캡처용 — `KASA_ADMIN_CAPTURE_DIR=<폴더> cargo test -p kasa-mcp admin_capture -- --ignored`
/// 격리 관문에서 가입 둘·관리자·비관리자 흐름을 돌리고 받은 HTML 을 그 폴더에 남긴다.
#[tokio::test]
#[ignore]
async fn admin_capture() {
    let out = std::path::PathBuf::from(std::env::var("KASA_ADMIN_CAPTURE_DIR").expect("KASA_ADMIN_CAPTURE_DIR"));
    std::fs::create_dir_all(&out).unwrap();
    let (gate, dir) = fixture(Some("one"));
    gate.oauth.resolve(&identity(Provider::Github, "900", "teacher-gh"), Some("one"), |_| true).unwrap();
    let (address, client) = serve(gate.clone()).await;
    let alice = sign_up(&gate, address, &client, identity(Provider::Google, "111", "alice@example.com"), "machine-alice").await;
    let bob = sign_up(&gate, address, &client, identity(Provider::Github, "222", "bob-gh"), "machine-bob").await;
    let _ = sign_up(&gate, address, &client, identity(Provider::Github, "222", "bob-gh"), "machine-bob-mini").await;
    // 앱이 관문으로 오간 몫을 흉내 낸다 — 실제 업링크 계량과 같은 길(`Meter::add`)로 넣는다.
    gate.usage.add(alice["account"].as_str().unwrap(), 2 * 3600 * 1000, 38 * 1024 * 1024, 412);
    gate.usage.add(bob["account"].as_str().unwrap(), 25 * 60 * 1000, 900 * 1024, 37);
    let login = client.get(format!("http://{address}/relay/admin")).send().await.unwrap().text().await.unwrap();
    std::fs::write(out.join("1-로그인-전.html"), login).unwrap();
    let denied = admin_login(&gate, address, &client, identity(Provider::Github, "222", "bob-gh")).await;
    std::fs::write(out.join("2-비관리자-거절.txt"), format!("HTTP {}\n\n{}", denied.status(), denied.text().await.unwrap())).unwrap();
    let anon = client.get(format!("http://{address}/relay/admin/accounts")).send().await.unwrap();
    std::fs::write(out.join("3-쿠키없이-목록.txt"), format!("HTTP {}\n\n{}", anon.status(), anon.text().await.unwrap())).unwrap();
    let signed_in = admin_login(&gate, address, &client, identity(Provider::Github, "900", "teacher-gh")).await;
    let cookie = first_cookie(&signed_in);
    let page = client.get(format!("http://{address}/relay/admin")).header(header::COOKIE, &cookie).send().await.unwrap().text().await.unwrap();
    std::fs::write(out.join("4-관리자-목록.html"), page).unwrap();
    // 실제 kasa-relay 로 이어 볼 상태(격리 앱 계정 화면용). 토큰이 있어 공유 폴더가 아닌 곳에만 둔다.
    if let Ok(keep) = std::env::var("KASA_ADMIN_CAPTURE_STATE") {
        let keep = std::path::PathBuf::from(keep);
        std::fs::create_dir_all(&keep).unwrap();
        gate.usage.save();
        for (from, to) in [
            ("state.json", "relay-state.json"),
            ("relay-oauth-identities.json", "relay-oauth-identities.json"),
            ("relay-usage.json", "relay-usage.json"),
            ("accounts.json", "relay-accounts.json"),
        ] {
            std::fs::copy(dir.join(from), keep.join(to)).unwrap();
        }
        std::fs::write(keep.join("alice.json"), alice.to_string()).unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}
