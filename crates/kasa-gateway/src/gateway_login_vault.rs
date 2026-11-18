//! `/relay/login-vault` — 웹 로그인 동기화의 잠긴 사본과 새 기기 등록 자리(docs/login-vault-sync.md).
//! 관문은 덩어리를 풀 열쇠가 없다. 계정은 기기 토큰이 정하고, 데스크톱만 쓴다 — 폰은 승인만 한다.
//! 로그에 남기는 것은 없다(항목 이름조차 HMAC 이지만 남길 까닭이 없다).

use super::*;
use kasa_device::login_vault::crypto::SealedKey;
use kasa_device::login_vault::server::{Approval, Error, Put, Store};
use serde::Deserialize;
use serde_json::json;

const MAX_BODY: usize = 1024 * 1024;

pub(super) fn routes() -> Router<Gate> {
    store_routes("/relay/login-vault", |gate| &gate.login_vault)
        .route("/relay/login-vault/joins", get(pending).post(open_join))
        .route("/relay/login-vault/joins/{id}", get(take_sealed))
        .route("/relay/login-vault/joins/{id}/approval", axum::routing::post(attach_approval))
        .route("/relay/login-vault/joins/{id}/sealed", axum::routing::post(post_sealed))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 쓸 수 있는 데스크톱 — `(기기 id, 계정)`.
fn desktop(gate: &Gate, headers: &axum::http::HeaderMap) -> Result<(String, String), axum::response::Response> {
    let Some((id, device)) = gate.device_of(headers) else {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    };
    let live = gate.devices.lock().unwrap().get(&id).is_some_and(|d| d.revoked_at.is_none());
    if !live || !gate.account_active(&device.account) {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    if device.kind != "desktop" {
        return Err(json_err(StatusCode::FORBIDDEN, "desktop_only"));
    }
    Ok((id, device.account))
}

fn refuse(error: Error) -> axum::response::Response {
    match error {
        Error::Invalid(code) => json_err(StatusCode::BAD_REQUEST, code),
        Error::KeyMismatch => json_err(StatusCode::CONFLICT, "key_mismatch"),
        Error::Exists(key_id) => (StatusCode::CONFLICT, axum::Json(json!({"ok": false, "error": "vault_exists", "key_id": key_id}))).into_response(),
        Error::NotFound => json_err(StatusCode::NOT_FOUND, "not_found"),
        Error::Forbidden => json_err(StatusCode::FORBIDDEN, "forbidden"),
        Error::BadSignature => json_err(StatusCode::FORBIDDEN, "bad_signature"),
        Error::AlreadySealed => json_err(StatusCode::CONFLICT, "already_sealed"),
        Error::TooMany => json_err(StatusCode::TOO_MANY_REQUESTS, "too_many"),
        Error::Storage => json_err(StatusCode::INTERNAL_SERVER_ERROR, "storage_unavailable"),
    }
}

fn ok(value: serde_json::Value) -> axum::response::Response {
    ([(header::CACHE_CONTROL, "no-store")], axum::Json(value)).into_response()
}

/// 본문을 받은 뒤 인증을 다시 본다 — 느린 업로드가 기기 폐기와 엇갈리지 않게.
async fn body<T: for<'de> Deserialize<'de>>(
    gate: &Gate,
    req: axum::extract::Request,
) -> Result<(String, String, T), axum::response::Response> {
    let headers = req.headers().clone();
    desktop(gate, &headers)?;
    let bytes = match tokio::time::timeout(Duration::from_secs(10), axum::body::to_bytes(req.into_body(), MAX_BODY)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return Err(json_err(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large")),
        Err(_) => return Err(json_err(StatusCode::REQUEST_TIMEOUT, "request_timeout")),
    };
    let value = serde_json::from_slice(&bytes).map_err(|_| json_err(StatusCode::BAD_REQUEST, "bad_request"))?;
    let (id, account) = desktop(gate, &headers)?;
    Ok((id, account, value))
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: u64,
}

/// 잠긴 사본의 받기·만들기·쓰기 창구. 로그인 금고와 북마크·탭 동기화(`/relay/browser-sync`)가 저마다의 보관소로 쓴다 —
/// 열쇠 표식·서명 공개키·한도가 보관소마다 따로라 한쪽의 서명이나 덩어리로 다른 쪽에 쓰지 못한다.
pub(super) fn store_routes(base: &'static str, pick: fn(&Gate) -> &Store) -> Router<Gate> {
    Router::new()
        .route(
            base,
            get(move |State(gate): State<Gate>, headers: axum::http::HeaderMap, query: axum::extract::Query<Since>| async move {
                fetch(pick(&gate), &gate, headers, query).await
            }),
        )
        .route(
            &format!("{base}/init"),
            axum::routing::post(move |State(gate): State<Gate>, req: axum::extract::Request| async move { init(pick(&gate), &gate, req).await }),
        )
        .route(
            &format!("{base}/items"),
            axum::routing::put(move |State(gate): State<Gate>, req: axum::extract::Request| async move { put(pick(&gate), &gate, req).await }),
        )
}

async fn fetch(store: &Store, gate: &Gate, headers: axum::http::HeaderMap, query: axum::extract::Query<Since>) -> axum::response::Response {
    let (_, account) = match desktop(gate, &headers) {
        Ok(who) => who,
        Err(response) => return response,
    };
    match store.fetch(&account, query.since, now_ms()) {
        Ok(fetched) => ok(serde_json::to_value(fetched).unwrap_or_default()),
        Err(error) => refuse(error),
    }
}

#[derive(Deserialize)]
struct Init {
    key_id: String,
    sign_pub: String,
}

async fn init(store: &Store, gate: &Gate, req: axum::extract::Request) -> axum::response::Response {
    let (device, account, init) = match body::<Init>(gate, req).await {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    let label = gate.devices.lock().unwrap().get(&device).map(|d| d.label.clone()).unwrap_or_default();
    match store.init(&account, &init.key_id, &init.sign_pub, &label, now_ms()) {
        Ok(()) => ok(json!({"ok": true})),
        Err(error) => refuse(error),
    }
}

#[derive(Deserialize)]
struct Items {
    key_id: String,
    puts: Vec<Put>,
    sig: String,
}

async fn put(store: &Store, gate: &Gate, req: axum::extract::Request) -> axum::response::Response {
    let (_, account, items) = match body::<Items>(gate, req).await {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    match store.put(&account, &items.key_id, &items.puts, &items.sig, now_ms()) {
        Ok(outcomes) => ok(json!({"outcomes": outcomes})),
        Err(error) => refuse(error),
    }
}

#[derive(Deserialize)]
struct OpenJoin {
    label: String,
    join_pub: String,
}

async fn open_join(State(gate): State<Gate>, req: axum::extract::Request) -> axum::response::Response {
    let (device, account, open) = match body::<OpenJoin>(&gate, req).await {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    match gate.login_vault.open_join(&account, &device, &open.label, &open.join_pub, now_ms()) {
        Ok(id) => ok(json!({"id": id})),
        Err(error) => refuse(error),
    }
}

async fn pending(State(gate): State<Gate>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (device, account) = match desktop(&gate, &headers) {
        Ok(who) => who,
        Err(response) => return response,
    };
    match gate.login_vault.pending_joins(&account, &device, now_ms()) {
        Ok(joins) => ok(json!({"joins": joins})),
        Err(error) => refuse(error),
    }
}

async fn attach_approval(State(gate): State<Gate>, AxPath(id): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let (device, account, approval) = match body::<Approval>(&gate, req).await {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    match gate.login_vault.attach_approval(&account, &device, &id, approval, now_ms()) {
        Ok(()) => ok(json!({"ok": true})),
        Err(error) => refuse(error),
    }
}

async fn post_sealed(State(gate): State<Gate>, AxPath(id): AxPath<String>, req: axum::extract::Request) -> axum::response::Response {
    let (device, account, sealed) = match body::<SealedKey>(&gate, req).await {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    match gate.login_vault.post_sealed(&account, &device, &id, sealed, now_ms()) {
        Ok(()) => ok(json!({"ok": true})),
        Err(error) => refuse(error),
    }
}

async fn take_sealed(State(gate): State<Gate>, AxPath(id): AxPath<String>, headers: axum::http::HeaderMap) -> axum::response::Response {
    let (device, account) = match desktop(&gate, &headers) {
        Ok(who) => who,
        Err(response) => return response,
    };
    match gate.login_vault.take_sealed(&account, &device, &id, now_ms()) {
        Ok(sealed) => ok(json!({"sealed": sealed})),
        Err(error) => refuse(error),
    }
}


#[cfg(test)]
mod tests {
    use super::super::tests::{account_gate, get_json, post, spawn_relay};
    use kasa_device::login_vault::crypto::{seal_for, JoinSecret, VaultKey};
    use kasa_device::login_vault::server::{put_message, Put};
    use serde_json::json;

    async fn put(addr: std::net::SocketAddr, token: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let reply = reqwest::Client::new().put(format!("http://{addr}/relay/login-vault/items")).bearer_auth(token).json(&body).send().await.unwrap();
        (reply.status().as_u16(), reply.json().await.unwrap_or_default())
    }

    fn signed(key: &VaultKey, puts: &[Put]) -> serde_json::Value {
        let key_id = key.key_id();
        json!({"key_id": key_id, "puts": puts, "sig": key.sign(&put_message("tester", &key_id, puts))})
    }

    /// 데스크톱 둘·폰 하나로 창구 그대로 — 금고 만들기·서명된 쓰기·등록 요청 나르기, 폰과 열쇠 없는 기기는 못 쓴다.
    #[tokio::test]
    async fn login_vault_over_http_for_two_desktops() {
        let dir = std::env::temp_dir().join(format!("kasa-login-vault-http-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let login = |machine: &'static str, kind: &'static str| async move {
            post(addr, "/relay/login", None, json!({"account":"tester","password":"correct horse","machine_id":machine,"kind":kind})).await.1
        };
        let mac = login("vault-mac", "desktop").await;
        let pc = login("vault-pc", "desktop").await;
        let phone = login("", "phone").await;
        let (mac_token, pc_token, phone_token) = (mac["token"].as_str().unwrap(), pc["token"].as_str().unwrap(), phone["token"].as_str().unwrap());

        assert_eq!(get_json(addr, "/relay/login-vault", phone_token).await.0, 403, "폰은 쓰지 않는다");
        assert_eq!(get_json(addr, "/relay/login-vault", "invalid").await.0, 401);
        let key = VaultKey::generate().unwrap();
        let other = VaultKey::generate().unwrap();
        assert_eq!(post(addr, "/relay/login-vault/init", Some(mac_token), json!({"key_id": key.key_id(), "sign_pub": key.sign_pub()})).await.0, 200);
        let (status, exists) = post(addr, "/relay/login-vault/init", Some(pc_token), json!({"key_id": other.key_id(), "sign_pub": other.sign_pub()})).await;
        assert_eq!((status, exists["error"].as_str()), (409, Some("vault_exists")));

        let id = "ab".repeat(32);
        let first = [Put { id: id.clone(), expected_rev: 0, blob: Some("c2VhbGVk".into()) }];
        let (status, out) = put(addr, mac_token, signed(&key, &first)).await;
        assert_eq!(status, 200, "{out}");
        assert_eq!(out["outcomes"][0]["result"], "ok");
        let (status, out) = put(addr, pc_token, signed(&key, &[Put { id: id.clone(), expected_rev: 0, blob: Some("b3RoZXI=".into()) }])).await;
        assert_eq!((status, out["outcomes"][0]["result"].as_str()), (200, Some("conflict")));
        // 열쇠 표식은 누구나 받아 본다 — 표식만 맞춘 쓰기는 서명에서 막힌다.
        let mut forged = signed(&other, &[Put { id: id.clone(), expected_rev: 1, blob: Some("eA==".into()) }]);
        forged["key_id"] = json!(key.key_id());
        let (status, refused) = put(addr, pc_token, forged).await;
        assert_eq!((status, refused["error"].as_str()), (403, Some("bad_signature")));
        let fetched = get_json(addr, "/relay/login-vault?since=0", pc_token).await.1;
        assert_eq!((fetched["revision"].as_u64(), fetched["items"][0]["blob"].as_str()), (Some(1), Some("c2VhbGVk")));
        assert!(fetched["now_ms"].as_u64().is_some_and(|n| n > 0), "관문 시계를 함께 준다");
        assert_eq!(get_json(addr, "/relay/login-vault?since=1", pc_token).await.1["items"], json!([]));

        // 새 기기(pc)가 등록을 열고 승인을 붙이면, 열쇠를 가진 기기(mac)만 그 요청을 보고 봉인을 올린다.
        let secret = JoinSecret::new().unwrap();
        let (_, opened) = post(addr, "/relay/login-vault/joins", Some(pc_token), json!({"label": "집 PC", "join_pub": secret.public_base64()})).await;
        let join = opened["id"].as_str().unwrap().to_string();
        assert_eq!(get_json(addr, "/relay/login-vault/joins", mac_token).await.1["joins"], json!([]));
        let approval = json!({"challenge": "{}", "key": "k", "sig": "s", "by_device": "phone"});
        assert_eq!(post(addr, &format!("/relay/login-vault/joins/{join}/approval"), Some(mac_token), approval.clone()).await.0, 403);
        assert_eq!(post(addr, &format!("/relay/login-vault/joins/{join}/approval"), Some(pc_token), approval).await.0, 200);
        assert_eq!(get_json(addr, "/relay/login-vault/joins", mac_token).await.1["joins"].as_array().map(Vec::len), Some(1));
        assert_eq!(get_json(addr, "/relay/login-vault/joins", pc_token).await.1["joins"], json!([]));
        assert_eq!(get_json(addr, &format!("/relay/login-vault/joins/{join}"), pc_token).await.1["sealed"], json!(null));
        let pc_id = pc["device_id"].as_str().unwrap();
        let sealed = serde_json::to_value(seal_for(&key, "tester", pc_id, &join, &secret.public_base64()).unwrap()).unwrap();
        let stranger = serde_json::to_value(seal_for(&other, "tester", pc_id, &join, &secret.public_base64()).unwrap()).unwrap();
        assert_eq!(post(addr, &format!("/relay/login-vault/joins/{join}/sealed"), Some(pc_token), sealed.clone()).await.0, 403);
        assert_eq!(post(addr, &format!("/relay/login-vault/joins/{join}/sealed"), Some(mac_token), stranger).await.0, 403, "금고 열쇠 없는 봉인");
        assert_eq!(post(addr, &format!("/relay/login-vault/joins/{join}/sealed"), Some(mac_token), sealed.clone()).await.0, 200);
        let (status, again) = post(addr, &format!("/relay/login-vault/joins/{join}/sealed"), Some(mac_token), sealed.clone()).await;
        assert_eq!((status, again["error"].as_str()), (409, Some("already_sealed")));
        assert_eq!(get_json(addr, &format!("/relay/login-vault/joins/{join}"), mac_token).await.0, 403);
        assert_eq!(get_json(addr, &format!("/relay/login-vault/joins/{join}"), pc_token).await.1["sealed"], sealed);
        assert_eq!(get_json(addr, &format!("/relay/login-vault/joins/{join}"), pc_token).await.0, 404, "봉인은 한 번만 내준다");

        // 폰 승인 창구가 등록 도전문을 받는다 — 폰에는 「로그인 금고」로 보인다.
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let text = kasa_device::login_vault::join::challenge("tester", pc_id, &"0f".repeat(16), now + 60_000, "집 PC", &[7u8; 32]);
        let (status, created) = post(addr, "/relay/approvals", Some(pc_token), json!({"kind": "secret", "challenge": text})).await;
        assert_eq!(status, 200, "{created}");
        assert_eq!(created["approval"]["tool"], "로그인 금고");

        let saved = std::fs::read_to_string(dir.join("login-vault").join("tester.json")).unwrap();
        assert!(saved.contains("c2VhbGVk") && !saved.contains(&secret.public_base64()), "등록 요청은 디스크에 남기지 않는다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 북마크·탭 동기화는 같은 창구 꼴의 다른 보관소다 — 금고 열쇠의 갈래로 만들고, 금고보다 큰 덩어리(탭 목록)를 받으며,
    /// 금고 서명으로는 쓰지 못한다.
    #[tokio::test]
    async fn browser_sync_is_its_own_store_beside_the_login_vault() {
        let dir = std::env::temp_dir().join(format!("kasa-browser-sync-http-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let mac = post(addr, "/relay/login", None, json!({"account":"tester","password":"correct horse","machine_id":"sync-mac","kind":"desktop"})).await.1;
        let phone = post(addr, "/relay/login", None, json!({"account":"tester","password":"correct horse","kind":"phone"})).await.1;
        let (token, phone_token) = (mac["token"].as_str().unwrap(), phone["token"].as_str().unwrap());
        let vault = VaultKey::generate().unwrap();
        let sync = vault.subkey("browser-sync");
        assert_eq!(get_json(addr, "/relay/browser-sync", phone_token).await.0, 403, "폰은 쓰지 않는다");
        assert_eq!(post(addr, "/relay/login-vault/init", Some(token), json!({"key_id": vault.key_id(), "sign_pub": vault.sign_pub()})).await.0, 200);
        assert_eq!(get_json(addr, "/relay/browser-sync", token).await.1["key_id"], "", "금고를 만들어도 동기화 기록은 따로다");
        assert_eq!(post(addr, "/relay/browser-sync/init", Some(token), json!({"key_id": sync.key_id(), "sign_pub": sync.sign_pub()})).await.0, 200);

        let big = "QUFB".repeat(12 * 1024);
        let id = "cd".repeat(32);
        let puts = [Put { id: id.clone(), expected_rev: 0, blob: Some(big.clone()) }];
        let body = |key: &VaultKey| json!({"key_id": sync.key_id(), "puts": puts, "sig": key.sign(&put_message("tester", &sync.key_id(), &puts))});
        let send = |body: serde_json::Value| async move {
            let reply = reqwest::Client::new().put(format!("http://{addr}/relay/browser-sync/items")).bearer_auth(token).json(&body).send().await.unwrap();
            (reply.status().as_u16(), reply.json::<serde_json::Value>().await.unwrap_or_default())
        };
        let (status, refused) = send(body(&vault)).await;
        assert_eq!((status, refused["error"].as_str()), (403, Some("bad_signature")), "금고 서명으로는 못 쓴다");
        let (status, out) = send(body(&sync)).await;
        assert_eq!((status, out["outcomes"][0]["result"].as_str()), (200, Some("ok")), "탭 목록만 한 덩어리를 받는다");
        let (status, _) = put(addr, token, signed(&vault, &[Put { id, expected_rev: 0, blob: Some(big) }])).await;
        assert_eq!(status, 400, "금고는 그대로 4KB 까지");
        assert!(dir.join("browser-sync").join("tester.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 앱 쪽 HTTP 클라이언트와 엔진을 진짜 관문 창구에 — 두 데스크톱이 잠긴 사본으로 맞춘다.
    #[tokio::test]
    async fn two_desktops_sync_through_the_real_routes() {
        use kasa_device::login_vault::engine::{Engine, Remote};
        use kasa_device::login_vault::http::Http;
        use kasa_device::login_vault::state::State;
        use kasa_device::login_vault::store::{LoginStore, MemoryStore};

        let dir = std::env::temp_dir().join(format!("kasa-login-vault-e2e-{}", uuid::Uuid::new_v4()));
        let addr = spawn_relay(account_gate(&dir)).await;
        let login = |machine: &'static str| async move {
            post(addr, "/relay/login", None, json!({"account":"tester","password":"correct horse","machine_id":machine,"kind":"desktop"})).await.1
        };
        let cred = |v: serde_json::Value| kasa_device::device_auth::DeviceCred {
            relay: format!("http://{addr}"),
            account: "tester".into(),
            device_id: v["device_id"].as_str().expect(&v.to_string()).into(),
            token: v["token"].as_str().unwrap().into(),
            display_name: None,
        };
        let (mac, pc) = (cred(login("vault-sync-mac").await), cred(login("vault-sync-pc").await));
        let synced = tokio::task::spawn_blocking(move || {
            let key = VaultKey::generate().unwrap();
            let (mac_http, pc_http) = (Http::new(&mac).unwrap(), Http::new(&pc).unwrap());
            mac_http.init(&key.key_id(), &key.sign_pub()).unwrap();
            let (mac_store, pc_store) = (MemoryStore::default(), MemoryStore::default());
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            mac_store.set_clock(now - 60_000);
            mac_store.write("mail.example", "me", "hunter2").unwrap();
            let pc_key = VaultKey::from_base64(&key.to_base64()).unwrap();
            let round = |key: &VaultKey, who: &kasa_device::device_auth::DeviceCred, store: &MemoryStore, http: &Http, state: &mut State, at: u64| {
                Engine { account: &who.account, device: &who.device_id, key, store, remote: http, now_ms: at, allow_bulk_delete: false }.round(state).unwrap()
            };
            let (mut mac_state, mut pc_state) = (State::default(), State::default());
            assert_eq!(round(&key, &mac, &mac_store, &mac_http, &mut mac_state, now).uploaded, 1);
            assert_eq!(round(&pc_key, &pc, &pc_store, &pc_http, &mut pc_state, now + 1_000).received, 1);
            pc_store.delete("mail.example", "me").unwrap();
            assert_eq!(round(&pc_key, &pc, &pc_store, &pc_http, &mut pc_state, now + 2_000).uploaded, 1);
            assert_eq!(round(&key, &mac, &mac_store, &mac_http, &mut mac_state, now + 3_000).tombstones, 1);
            mac_store.snapshot().is_empty() && pc_store.snapshot().is_empty()
        })
        .await
        .unwrap();
        assert!(synced, "지운 것도 넘어간다");
        let saved = std::fs::read_to_string(dir.join("login-vault").join("tester.json")).unwrap();
        assert!(!saved.contains("hunter2") && !saved.contains("mail.example"), "관문 디스크에 평문이 없다");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
