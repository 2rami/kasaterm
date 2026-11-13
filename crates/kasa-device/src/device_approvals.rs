//! 비밀 승인의 데스크톱 쪽(docs/secret-faceid-approval.md) — 키체인 비밀 요청을 관문에 올리고 폰의 결정을 기다린다.
//! 도구 권한은 관문에 올리지 않는다(2026-11-13): 폰 알림은 칸의 승인 대기 알림 하나로만 가고, 고르기는 그 칸에서 한다.
//! 결정 동작은 이 기기에 없다 — 같은 기기의 에이전트가 자기 요청을 허락할 길이 없다.

use super::*;
use std::sync::OnceLock;

/// 쓸 로그인. 격리 실행은 사람 계정으로 떨어지지 않게 막되, 시험 기기 파일을 직접 준 리그는 허용한다.
fn credential() -> anyhow::Result<DeviceCred> {
    let explicit = std::env::var_os("KASATERM_DEVICE_FILE").is_some();
    anyhow::ensure!(sync_environment_allowed() || explicit, "isolated_run");
    current().ok_or_else(|| anyhow::anyhow!("signed_out"))
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(40))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

fn valid_id(id: &str) -> anyhow::Result<&str> {
    anyhow::ensure!(
        id.len() == 36 && id.starts_with("apv_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid_request"
    );
    Ok(id)
}

async fn call(cred: &DeviceCred, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
    let mut req = http().request(method, format!("{}/relay/approvals{path}", cred.relay.trim_end_matches('/'))).bearer_auth(&cred.token);
    if let Some(body) = body {
        req = req.json(&body);
    }
    let response = req.send().await.map_err(|_| anyhow::anyhow!("gateway_unreachable"))?;
    let status = response.status();
    if status.as_u16() == 404 && path.is_empty() {
        anyhow::bail!("update_required");
    }
    let value: Value = response.json().await.map_err(|_| anyhow::anyhow!("invalid_response"))?;
    if status.as_u16() == 401 && value["error"] == "unauthorized" {
        reject_token(&cred.token);
    }
    if !status.is_success() {
        // 닫힌·만료된 요청은 그 상태를 함께 준다 — 화면이 「어디서 닫혔나」를 보이게.
        if let Some(approval) = value.get("approval") {
            let code = value["error"].as_str().unwrap_or("request_failed").to_string();
            return Err(Refused { code, approval: approval.clone() }.into());
        }
        anyhow::bail!("{}", value["error"].as_str().unwrap_or("request_failed"));
    }
    Ok(value)
}

/// 비밀 요청 하나를 올린다(docs/secret-faceid-approval.md). 칸·학생·명령·참조는 도전값 안에만 있다.
pub async fn create_secret(challenge: &str) -> anyhow::Result<Value> {
    let cred = credential()?;
    let body = json!({"kind": "secret", "challenge": challenge});
    Ok(call(&cred, reqwest::Method::POST, "", Some(body)).await?["approval"].clone())
}

/// 계정의 살아 있는 폰 열쇠(폐기된 폰은 관문이 뺀다).
pub async fn keys() -> anyhow::Result<Vec<Value>> {
    let cred = credential()?;
    Ok(call(&cred, reqwest::Method::GET, "/keys", None).await?["keys"].as_array().cloned().unwrap_or_default())
}

/// 이 기기의 (계정, 기기 id) — 비밀 요청 도전값이 그 둘에 묶인다.
pub fn identity() -> anyhow::Result<(String, String)> {
    let cred = credential()?;
    Ok((cred.account, cred.device_id))
}

/// 결정·만료까지 최대 `wait` 초 붙든다. 닫혔으면 그 상태를, 아니면 `pending` 을 준다.
pub async fn wait_one(id: &str, wait: u64) -> anyhow::Result<Value> {
    let cred = credential()?;
    let id = valid_id(id)?;
    Ok(call(&cred, reqwest::Method::GET, &format!("/{id}?wait={}", wait.min(25)), None).await?["approval"].clone())
}

/// 이 기기가 낸 요청을 닫는다 — 원래 칸에서 답했거나(`local`) 칸·세션이 사라졌을 때(`gone`).
pub async fn cancel(id: &str, reason: &str) -> anyhow::Result<Value> {
    let cred = credential()?;
    let id = valid_id(id)?;
    Ok(call(&cred, reqwest::Method::POST, &format!("/{id}/cancel"), Some(json!({"reason": reason}))).await?["approval"].clone())
}

/// 관문이 거절하며 그 요청의 지금 상태를 함께 준 것(닫힌·만료된 요청). 글은 오류 코드.
#[derive(Debug)]
pub struct Refused {
    pub code: String,
    pub approval: Value,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}

impl std::error::Error for Refused {}

/// 오류에 실려 온 요청 상태 — 없으면 None.
pub fn closed_view(error: &anyhow::Error) -> Option<Value> {
    error.downcast_ref::<Refused>().map(|r| r.approval.clone())
}

/// 런타임이 없는 GUI 뒷실에서 부르는 길 — 부를 때마다 작은 런타임 하나.
fn blocking<T>(work: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(work)
}

pub fn create_secret_blocking(challenge: &str) -> anyhow::Result<Value> {
    blocking(create_secret(challenge))
}

pub fn wait_one_blocking(id: &str, wait: u64) -> anyhow::Result<Value> {
    blocking(wait_one(id, wait))
}

pub fn cancel_blocking(id: &str, reason: &str) -> anyhow::Result<Value> {
    blocking(cancel(id, reason))
}

pub fn keys_blocking() -> anyhow::Result<Vec<Value>> {
    blocking(keys())
}
