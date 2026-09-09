//! Resolve source-local development URLs before opening a selected browser.
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none()).build()?)
}

fn target_base(machine: &str) -> Result<String> {
    crate::machines::find_route(machine).map(|m| m.base)
        .context("선택한 기기의 카사텀 연결을 찾지 못했어요")
}

async fn response_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let value: Value = serde_json::from_str(&response.text().await?)
        .context("대상 카사텀을 새 버전으로 업데이트해야 해요")?;
    anyhow::ensure!(status.is_success() && value["ok"] == true, "{}",
        value["error"].as_str().unwrap_or("브라우저 기기에 연결하지 못했어요"));
    Ok(value)
}

pub async fn resolve_url(url: &str, selected: &str) -> Result<String> {
    if selected.is_empty() || !crate::browser_route::is_loopback_url(url) {
        return Ok(url.to_string());
    }
    let base = target_base(selected)?;
    let body = json!({"source_machine": crate::machines::self_label(),
        "source_machine_id": crate::mobile::machine_identity(), "url": url});
    let mut request = client()?.post(format!("{}/browser/resolve-localhost", base.trim_end_matches('/')))
        .header("content-type", "application/json").body(body.to_string());
    if let Some(token) = crate::remote::connection_auth_token(&base) {
        request = request.header("x-kasa-token", token);
    }
    let value = response_json(request.send().await?).await?;
    value["url"].as_str().map(str::to_string).context("대상 기기가 열 주소를 보내지 않았어요")
}

/// Only the selected target is addressed. No viewer broadcast or local fallback.
pub async fn open_selected(url: &str, selected: &str) -> Result<()> {
    anyhow::ensure!(!selected.is_empty(), "remote browser target required");
    let resolved = resolve_url(url, selected).await?;
    anyhow::ensure!(crate::machines::kasachrome_machine() == selected,
        "브라우저 기기가 바뀌었어요. 다시 열어 주세요");
    let base = target_base(selected)?;
    let mut request = client()?.get(format!("{}/open-url", base.trim_end_matches('/')))
        .query(&[("url", resolved.as_str()), ("local", "1")]);
    if let Some(token) = crate::remote::connection_auth_token(&base) {
        request = request.header("x-kasa-token", token);
    }
    response_json(request.send().await?).await?;
    Ok(())
}

pub fn open_selected_blocking(url: &str, selected: &str) -> Result<()> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()?
        .block_on(open_selected(url, selected))
}
