//! 이 기기 계정의 프로필(`/relay/profile`) — 설정 「계정」이 닉네임·프사·로그인 아이디·비밀번호를
//! 바꾼다. 비밀번호는 요청 한 번에만 실리고 어디에도 남지 않는다.

use super::*;

/// 화면에 올릴 프사 한 장의 한 변. 관문에도 이 크기로 줄여 올린다.
pub const AVATAR_PX: u32 = 256;
const MAX_PICTURE: usize = 2 * 1024 * 1024;

fn signed_in() -> anyhow::Result<DeviceCred> {
    anyhow::ensure!(sync_environment_allowed(), "isolated_run");
    current().ok_or_else(|| anyhow::anyhow!("signed_out"))
}

async fn send(request: reqwest::RequestBuilder, credential: &DeviceCred) -> anyhow::Result<Value> {
    let response = request
        .bearer_auth(&credential.token)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("gateway_unreachable"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        anyhow::bail!("update_required");
    }
    let value: Value = response.json().await.map_err(|_| anyhow::anyhow!("invalid_response"))?;
    if status.as_u16() == 401 && value["error"] == "unauthorized" {
        reject_token(&credential.token);
    }
    if !status.is_success() {
        anyhow::bail!("{}", value["error"].as_str().unwrap_or("request_failed"));
    }
    Ok(value)
}

/// 바뀐 닉네임을 이 기기 로그인 기록과 연결 상태에 바로 싣는다 — 상태줄·계정 메뉴가 같은 이름을 쓴다.
fn remember_name(credential: &DeviceCred, profile: &Value) {
    let name = display_label(profile["display_name"].as_str());
    crate::uplink::set_display_name(name.clone());
    if credential.display_name != name {
        if let Some(mut saved) = current().filter(|saved| saved.token == credential.token) {
            saved.display_name = name;
            let _ = save(&saved);
        }
    }
}

pub(super) async fn handle(op: &str, params: &Value) -> anyhow::Result<Value> {
    let credential = signed_in()?;
    let url = |path: &str| format!("{}/relay/profile{path}", credential.relay);
    let http = client()?;
    let value = match op {
        "profile" => send(http.get(url("")), &credential).await?,
        "profile_update" => {
            let mut body = serde_json::Map::new();
            for name in ["nickname", "avatar"] {
                if let Some(value) = params.get(name).filter(|value| value.is_string()) {
                    body.insert(name.into(), value.clone());
                }
            }
            send(http.patch(url("")).json(&body), &credential).await?
        }
        "avatar_remove" => send(http.delete(url("/avatar")), &credential).await?,
        "login_change" | "password_change" => {
            let mut body = json!({"password": params["password"].as_str().unwrap_or("")});
            if op == "login_change" {
                body["login"] = json!(params["login"].as_str().unwrap_or("").trim().to_lowercase());
            } else {
                body["new_password"] = json!(params["new_password"].as_str().unwrap_or(""));
            }
            let path = if op == "login_change" { "/login" } else { "/password" };
            send(http.post(url(path)).json(&body), &credential).await?
        }
        other => anyhow::bail!("모르는 동작이에요: {other}"),
    };
    if value["display_name"].is_string() || value.get("display_name").is_some_and(Value::is_null) {
        remember_name(&credential, &value);
    }
    Ok(value)
}

/// 줄여 둔 그림(PNG·JPEG·WebP)을 이 계정 프사로 올린다.
pub fn upload(bytes: Vec<u8>) -> anyhow::Result<Value> {
    std::thread::spawn(move || -> anyhow::Result<Value> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            let credential = signed_in()?;
            let request = client()?
                .put(format!("{}/relay/profile/avatar", credential.relay))
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes);
            send(request, &credential).await
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("request_failed"))?
}

/// 프사 그림을 받는다 — 올린 그림은 관문에서, 공급자 사진은 그 이미지 호스트에서만.
pub fn picture(avatar: &Value) -> anyhow::Result<Vec<u8>> {
    let avatar = avatar.clone();
    std::thread::spawn(move || -> anyhow::Result<Vec<u8>> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            let http = client()?;
            let request = if avatar["source"] == "upload" {
                let credential = signed_in()?;
                http.get(format!("{}/relay/profile/avatar", credential.relay)).bearer_auth(credential.token)
            } else {
                let url = crate::oauth_accounts::picture_url(avatar["url"].as_str())
                    .ok_or_else(|| anyhow::anyhow!("invalid_picture"))?;
                http.get(url)
            };
            let mut response = request.send().await?;
            anyhow::ensure!(response.status().is_success(), "picture_unavailable");
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                anyhow::ensure!(bytes.len() + chunk.len() <= MAX_PICTURE, "picture_too_large");
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("picture_unavailable"))?
}
