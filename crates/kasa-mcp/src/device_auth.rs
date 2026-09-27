//! 이 기기의 관문 로그인 — 관문에 아이디·비밀번호로 한 번 로그인해 받은 **기기 토큰**을
//! `~/.config/kasaterm/device.json`(0600)에 둔다. 비밀번호는 남기지 않는다.
//!
//! 토큰이 있으면 업링크가 hello 에 실어 관문에 「이 계정의 이 기계」로 붙는다(`uplink.rs`).
//! 관문 쪽은 `gateway.rs`·`relay_auth.rs`. 로그인은 설정 화면이나 `kasaterm-cli login` 이
//! 소켓(`relay.account`)으로 부른다 — 화면 없는 기기(미니·윈도우)도 같은 길이다.
//!
//! 윈도우는 파일 권한을 따로 좁히지 않는다 — 사용자 프로필 폴더의 ACL 을 물려받는다.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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

fn clear() {
    if let Some(p) = path() {
        let _ = std::fs::remove_file(p);
    }
}

fn client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

async fn login(account: &str, password: &str) -> anyhow::Result<DeviceCred> {
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
    save(&cred)?;
    crate::uplink::poke();
    Ok(cred)
}

async fn logout() -> anyhow::Result<()> {
    if let Some(c) = current() {
        // 관문이 안 받아도 이 기기에서는 지운다 — 남은 토큰은 다른 기기에서 끊을 수 있다.
        let _ = client()?.post(format!("{}/relay/logout", c.relay)).bearer_auth(&c.token).send().await;
    }
    clear();
    crate::uplink::poke();
    Ok(())
}

async fn devices() -> anyhow::Result<Value> {
    let c = current().ok_or_else(|| anyhow::anyhow!("로그인돼 있지 않아요"))?;
    let res = client()?.get(format!("{}/relay/devices", c.relay)).bearer_auth(&c.token).send().await?;
    if res.status().as_u16() == 401 {
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
    Ok(())
}

/// 이 기기의 로그인 상태 — 토큰 값은 절대 싣지 않는다.
pub fn status() -> Value {
    let up = crate::uplink::status();
    let cred = current();
    json!({
        "gateway": crate::mobile::gateway(),
        "logged_in": cred.is_some(),
        "account": cred.as_ref().map(|c| c.account.clone()),
        "device_id": cred.as_ref().map(|c| c.device_id.clone()),
        "connected": up.connected,
        "connected_as": up.account,
        "auth_error": up.auth_error,
    })
}

/// 소켓 `relay.account` — `{op: login|logout|devices|revoke|status, …}`. 소켓 핸들러는 동기라
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
