//! 이 기기의 관문 로그인 — 관문에 아이디·비밀번호로 한 번 로그인해 받은 **기기 토큰**을
//! `~/.config/kasaterm/device.json`(0600)에 둔다. 비밀번호는 남기지 않는다.
//!
//! 토큰이 있으면 업링크가 hello 에 실어 관문에 「이 계정의 이 기계」로 붙는다(`uplink.rs`).
//! 관문 쪽은 `gateway.rs`·`relay_auth.rs`. 로그인은 설정 화면이나 `kasaterm-cli login` 이
//! 소켓(`relay.account`)으로 부른다 — 화면 없는 기기(미니·윈도우)도 같은 길이다.
//!
//! 윈도우는 파일 권한을 따로 좁히지 않는다 — 사용자 프로필 폴더의 ACL 을 물려받는다.

use std::path::PathBuf;
use std::sync::{Mutex, atomic::{AtomicU64, Ordering}};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[path = "device_oauth.rs"]
mod oauth;

static CREDENTIALS: Mutex<()> = Mutex::new(());
static EPOCH: AtomicU64 = AtomicU64::new(0);
static REJECTED: Mutex<Option<String>> = Mutex::new(None);

// UI caches can discard old-account content without reading credentials on the render thread.
pub fn change_epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}

pub fn cancel_oauth() {
    if let Ok(_guard) = CREDENTIALS.lock() {
        EPOCH.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    epoch: u64,
    identity: String,
}

impl Stamp {
    fn accepts(&self, credential: &DeviceCred, gateway: Option<&str>, epoch: u64) -> bool {
        self.epoch == epoch && self.identity == identity(credential)
            && gateway.is_some_and(|gateway| gateway.trim_end_matches('/') == credential.relay.trim_end_matches('/'))
    }
}

#[cfg(test)]
impl Stamp {
    pub(crate) fn fixture() -> Self { Self { epoch: 0, identity: "fixture".into() } }
    pub(crate) fn fixture_at(epoch: u64) -> Self { Self { epoch, identity: "fixture".into() } }
}

fn identity(credential: &DeviceCred) -> String {
    crate::relay_auth::token_hash(&format!("{}\0{}\0{}\0{}", credential.relay, credential.account, credential.device_id, credential.token))
}

fn sync_isolated(has_env: impl Fn(&str) -> bool) -> bool {
    ["KASATERM_WINDOW_SIZE", "KASATERM_WINDOW_POS", "KASATERM_AUTOQUIT_MS",
        "KASATERM_SETTINGS_FILE", "KASATERM_DEVICE_FILE", "KASATERM_MACHINES_FILE"]
        .iter().any(|name| has_env(name))
}

pub(crate) fn sync_environment_allowed() -> bool {
    // A fixture must never fall back to the user's saved account or provider identities.
    !cfg!(test) && !sync_isolated(|name| std::env::var_os(name).is_some())
}

pub(crate) fn capture() -> Option<(DeviceCred, Stamp)> {
    if !sync_environment_allowed() { return None; }
    let _guard = CREDENTIALS.lock().ok()?;
    let gateway = crate::mobile::gateway()?;
    let credential = for_gateway(&gateway)?;
    let identity = identity(&credential);
    if REJECTED.lock().ok()?.as_deref() == Some(identity.as_str()) { return None; }
    Some((credential, Stamp { epoch: EPOCH.load(Ordering::Acquire), identity }))
}

pub(crate) fn with_current<T>(stamp: &Stamp, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if !sync_environment_allowed() { return Err("account synchronization disabled in isolated run".into()); }
    let _guard = CREDENTIALS.lock().map_err(|_| "credential lock unavailable")?;
    let credential = current().ok_or("account changed")?;
    if !stamp.accepts(&credential, crate::mobile::gateway().as_deref(), EPOCH.load(Ordering::Acquire)) {
        return Err("account changed".into());
    }
    if REJECTED.lock().map_err(|_| "credential lock unavailable")?.as_deref() == Some(stamp.identity.as_str()) {
        return Err("authentication expired".into());
    }
    operation()
}

/// Narrow check for other crates: the stamp still names the signed-in device account.
/// Callers that need to act under the credential stay inside this crate via `with_current`.
pub fn stamp_is_current(stamp: &Stamp) -> bool {
    with_current(stamp, || Ok(())).is_ok()
}

pub(crate) fn reject(stamp: &Stamp) {
    reject_if(stamp, || true);
}

pub(crate) fn reject_if(stamp: &Stamp, relevant: impl FnOnce() -> bool) {
    let _ = with_current(stamp, || {
        if !relevant() { return Ok(()); }
        if let Ok(mut rejected) = REJECTED.lock() { *rejected = Some(stamp.identity.clone()); }
        EPOCH.fetch_add(1, Ordering::AcqRel);
        crate::agent_accounts::clear_cache();
        Ok(())
    });
}

pub(crate) fn reject_token(token: &str) {
    if let Some((credential, stamp)) = capture() {
        if credential.token == token { reject(&stamp); }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeviceCred {
    /// 로그인한 관문 주소. 관문을 바꾸면 이 토큰은 안 쓴다.
    pub relay: String,
    pub account: String,
    pub device_id: String,
    pub token: String,
}

fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KASATERM_DEVICE_FILE") {
        return Some(PathBuf::from(p));
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/device.json"))
}

pub fn current() -> Option<DeviceCred> {
    let raw = std::fs::read_to_string(path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 지금 관문에 쓸 수 있는 로그인 — 다른 관문에서 받은 토큰은 안 낸다.
pub fn for_gateway(gateway: &str) -> Option<DeviceCred> {
    current().filter(|c| c.relay.trim_end_matches('/') == gateway.trim_end_matches('/'))
}

fn save(c: &DeviceCred) -> anyhow::Result<()> {
    let p = path().ok_or_else(|| anyhow::anyhow!("설정 폴더를 못 찾았어요"))?;
    let body = serde_json::to_string_pretty(c)?;
    crate::relay_auth::write_private(&p, &body)?;
    Ok(())
}

fn clear() -> std::io::Result<()> {
    if let Some(p) = path() {
        match std::fs::remove_file(p) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn login(account: &str, password: &str) -> anyhow::Result<DeviceCred> {
    let (epoch, previous) = {
        let _guard = CREDENTIALS.lock().map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        crate::agent_accounts::clear_cache();
        (EPOCH.fetch_add(1, Ordering::AcqRel) + 1, current())
    };
    let gateway = crate::mobile::gateway().ok_or_else(|| anyhow::anyhow!("관문이 꺼져 있어요"))?;
    let machine_id = crate::mobile::machine_identity().ok_or_else(|| anyhow::anyhow!("이 기계의 id 를 못 만들었어요"))?;
    let res = client()?
        .post(format!("{gateway}/relay/login"))
        .json(&json!({
            "account": account, "password": password, "kind": "desktop",
            "label": crate::mobile::machine_name(), "machine_id": machine_id,
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("관문 {gateway} 에 못 닿았어요: {e}"))?;
    let status = res.status().as_u16();
    let v: Value = res.json().await.unwrap_or_default();
    match status {
        200 => {}
        401 => anyhow::bail!("아이디나 비밀번호가 달라요"),
        429 => anyhow::bail!(
            "너무 여러 번 시도했어요 — {}초 뒤에 다시 해 주세요",
            v["retry_after_secs"].as_u64().unwrap_or(60)
        ),
        _ => anyhow::bail!("관문이 로그인을 받지 않았어요({status}: {})", v["error"].as_str().unwrap_or("?")),
    }
    let cred = DeviceCred {
        relay: gateway,
        account: v["account"].as_str().unwrap_or(account).to_string(),
        device_id: v["device_id"].as_str().unwrap_or_default().to_string(),
        token: v["token"].as_str().ok_or_else(|| anyhow::anyhow!("관문이 토큰을 안 줬어요"))?.to_string(),
    };
    {
        let _guard = CREDENTIALS.lock().map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        anyhow::ensure!(EPOCH.load(Ordering::Acquire) == epoch, "login superseded by another account change");
        save(&cred)?;
        if let Ok(mut rejected) = REJECTED.lock() { *rejected = None; }
        crate::account_sync::credentials_changed(previous.as_ref(), Some(&cred));
    }
    crate::uplink::poke();
    crate::account_sync::spawn();
    crate::account_sync::poke();
    Ok(cred)
}

async fn logout() -> anyhow::Result<()> {
    let previous = {
        let _guard = CREDENTIALS.lock().map_err(|_| anyhow::anyhow!("credential lock unavailable"))?;
        EPOCH.fetch_add(1, Ordering::AcqRel);
        crate::agent_accounts::clear_cache();
        let previous = current();
        clear()?;
        crate::account_sync::credentials_changed(previous.as_ref(), None);
        previous
    };
    crate::uplink::poke();
    crate::account_sync::poke();
    if let Some(c) = previous {
        // 관문이 안 받아도 이 기기에서는 지운다 — 남은 토큰은 다른 기기에서 끊을 수 있다.
        let _ = client()?.post(format!("{}/relay/logout", c.relay)).bearer_auth(&c.token).send().await;
    }
    Ok(())
}

async fn devices() -> anyhow::Result<Value> {
    let c = current().ok_or_else(|| anyhow::anyhow!("로그인돼 있지 않아요"))?;
    let res = client()?.get(format!("{}/relay/devices", c.relay)).bearer_auth(&c.token).send().await?;
    if res.status().as_u16() == 401 {
        reject_token(&c.token);
        anyhow::bail!("로그인이 풀렸어요 — 다시 로그인해 주세요");
    }
    Ok(res.json().await?)
}

async fn revoke(device_id: &str) -> anyhow::Result<()> {
    let c = current().ok_or_else(|| anyhow::anyhow!("로그인돼 있지 않아요"))?;
    let res = client()?
        .post(format!("{}/relay/devices/{device_id}/revoke", c.relay))
        .bearer_auth(&c.token)
        .send()
        .await?;
    if !res.status().is_success() {
        anyhow::bail!("그 기기를 끊지 못했어요({})", res.status());
    }
    if device_id == c.device_id { reject_token(&c.token); }
    Ok(())
}

/// 이 기기의 로그인 상태 — 토큰 값은 절대 싣지 않는다.
pub fn status() -> Value {
    let up = crate::uplink::status();
    let cred = current();
    let rejected = REJECTED.lock().map(|rejected| {
        cred.as_ref().is_some_and(|cred| rejected.as_deref() == Some(identity(cred).as_str()))
    }).unwrap_or(true);
    let mut status = status_from(cred.as_ref(), crate::mobile::gateway().as_deref(), &up, rejected);
    status["sync"] = crate::account_sync::status();
    status
}

fn status_from(cred: Option<&DeviceCred>, gateway: Option<&str>, up: &crate::uplink::Status, rejected: bool) -> Value {
    let matching = cred.zip(gateway).is_some_and(|(c, gateway)| {
        c.relay.trim_end_matches('/') == gateway.trim_end_matches('/')
    });
    let auth_error = up.auth_error.as_deref().or(rejected.then_some("authentication_expired"));
    let authenticated = matching && up.connected && auth_error.is_none()
        && cred.is_some_and(|c| up.account.as_deref() == Some(c.account.as_str()));
    let state = if gateway.is_none() {
        "gateway_off"
    } else if cred.is_none() {
        "signed_out"
    } else if !matching {
        "gateway_changed"
    } else if auth_error.is_some() {
        "reauth_required"
    } else if authenticated {
        "connected"
    } else {
        "connecting"
    };
    json!({
        "gateway": gateway,
        "credential_saved": cred.is_some(),
        "logged_in": matching && auth_error.is_none(),
        "authenticated": authenticated,
        "state": state,
        "account": cred.as_ref().map(|c| c.account.clone()),
        "device_id": cred.as_ref().map(|c| c.device_id.clone()),
        "connected": up.connected,
        "connected_as": up.account,
        "auth_error": auth_error,
        "connection_error": up.last_error,
    })
}

/// 소켓 `relay.account` — `{op: login|logout|devices|revoke|status|agents, …}`. 소켓 핸들러는 동기라
/// 따로 스레드를 세워 그 안에서만 런타임을 돌린다(`tell_service::remote` 와 같은 방식).
pub fn handle(params: &Value) -> anyhow::Result<Value> {
    let op = params["op"].as_str().unwrap_or("status").to_string();
    if op == "status" {
        return Ok(status());
    }
    let params = params.clone();
    std::thread::spawn(move || -> anyhow::Result<Value> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            match op.as_str() {
                "oauth_providers" => oauth::providers().await,
                "oauth_start" => oauth::start(&params).await,
                "oauth_poll" => oauth::poll(&params).await,
                "oauth_cancel" => oauth::cancel(&params).await,
                "login" => {
                    let account = params["account"].as_str().unwrap_or("").trim().to_lowercase();
                    let password = params["password"].as_str().unwrap_or("");
                    let c = login(&account, password).await?;
                    Ok(json!({ "ok": true, "account": c.account, "device_id": c.device_id }))
                }
                "logout" => logout().await.map(|_| json!({ "ok": true })),
                "devices" => devices().await,
                "revoke" => {
                    let id = params["device_id"].as_str().ok_or_else(|| anyhow::anyhow!("device_id 가 필요해요"))?;
                    revoke(id).await.map(|_| json!({ "ok": true }))
                }
                // 이 기기 슬롯을 관문에 올리고 합친 코딩 에이전트 계정 목록을 받는다.
                "agents" => {
                    let list = crate::agent_accounts::sync(&crate::agent_accounts::local_snapshot()).await?;
                    Ok(json!({ "ok": true, "accounts": list }))
                }
                other => anyhow::bail!("모르는 동작이에요: {other}"),
            }
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("관문 로그인 작업이 멈췄어요"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_stamp_rejects_gateway_account_token_and_login_epoch_changes() {
        let cred = DeviceCred { relay:"https://relay.example".into(), account:"one".into(),
            device_id:"fixture-device".into(), token:"fixture-token".into() };
        let stamp = Stamp { epoch: 3, identity: identity(&cred) };
        assert!(stamp.accepts(&cred, Some("https://relay.example/"), 3));
        assert!(!stamp.accepts(&cred, None, 3));
        assert!(!stamp.accepts(&cred, Some("https://other.example"), 3));
        assert!(!stamp.accepts(&cred, Some(&cred.relay), 4));
        for changed in [
            DeviceCred { account:"two".into(), ..cred.clone() },
            DeviceCred { token:"rotated-fixture".into(), ..cred.clone() },
            DeviceCred { device_id:"replacement-device".into(), ..cred.clone() },
            DeviceCred { relay:"https://other.example".into(), ..cred.clone() },
        ] {
            assert!(!stamp.accepts(&changed, Some(&changed.relay), 3));
        }
    }

    #[test]
    fn every_qa_entry_point_disables_implicit_account_sync() {
        assert!(!sync_isolated(|_| false));
        for variable in ["KASATERM_WINDOW_SIZE", "KASATERM_WINDOW_POS", "KASATERM_AUTOQUIT_MS",
            "KASATERM_SETTINGS_FILE", "KASATERM_DEVICE_FILE", "KASATERM_MACHINES_FILE"] {
            assert!(sync_isolated(|name| name == variable), "{variable}");
        }
        assert!(capture().is_none());
    }

    #[test]
    fn saved_credentials_are_not_a_confirmed_connection() {
        let cred = DeviceCred { relay: "https://relay.example".into(), account: "sample".into(),
            device_id: "device-one".into(), token: "never-render-this-token".into() };
        let mut up = crate::uplink::Status::default();
        let state = status_from(Some(&cred), Some("https://relay.example/"), &up, false);
        assert_eq!(state["state"], "connecting");
        assert_eq!(state["credential_saved"], true);
        assert_eq!(state["authenticated"], false);
        up.connected = true;
        assert_eq!(status_from(Some(&cred), Some(&cred.relay), &up, false)["authenticated"], false);
        up.account = Some(cred.account.clone());
        assert_eq!(status_from(Some(&cred), Some(&cred.relay), &up, false)["state"], "connected");
        let rejected_http = status_from(Some(&cred), Some(&cred.relay), &up, true);
        assert_eq!(rejected_http["state"], "reauth_required");
        assert_eq!(rejected_http["logged_in"], false);
        assert_eq!(rejected_http["authenticated"], false);
        assert_eq!(rejected_http["auth_error"], "authentication_expired");
        up.auth_error = Some("rejected".into());
        let rejected = status_from(Some(&cred), Some(&cred.relay), &up, false);
        assert_eq!(rejected["logged_in"], false);
        assert_eq!(rejected["state"], "reauth_required");
        assert!(!rejected.to_string().contains(&cred.token));
        assert_eq!(status_from(Some(&cred), Some("https://other.example"), &up, false)["state"], "gateway_changed");
        assert_eq!(status_from(None, Some(&cred.relay), &up, false)["state"], "signed_out");
    }

    #[test]
    fn credential_file_round_trips_privately() {
        let dir = std::env::temp_dir().join(format!("kasa-dev-{}", uuid::Uuid::new_v4()));
        let p = dir.join("device.json");
        let c = DeviceCred {
            relay: "https://relay.example".into(),
            account: "geno".into(),
            device_id: "dev_1".into(),
            token: "kdt_x".into(),
        };
        crate::relay_auth::write_private(&p, &serde_json::to_string(&c).unwrap()).unwrap();
        let back: DeviceCred = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back, c);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
