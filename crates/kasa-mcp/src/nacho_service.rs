//! 나쵸 인박스 보고의 **라우팅** — 나쵸가 이 기계에 살면 인박스에 바로 넣고, 다른
//! 기계면 그 기계의 `POST /nacho/report` 로 넘긴다. 저장·중복·깨우기는 전부
//! `kasa_socket::nacho_inbox` 가 하고 여기는 「어느 기계냐」만 가른다(tell 과 같은 분업).
//!
//! 학생은 `KASATERM_ORIGIN_MACHINE`(나쵸가 사는 기계의 machine_id 또는 라벨)을
//! `machine_id` 로 싣는다. 비면 이 기계다 — 나쵸와 학생이 한 기계에 사는 흔한 경우.
use anyhow::{bail, ensure, Context, Result};
use kasa_socket::nacho_inbox;
use serde_json::{json, Value};

pub fn submit(params: &Value) -> Result<Value> {
    ensure!(params.is_object(), "nacho.report requires an object");
    let local = crate::board_service::local_id()?;
    let self_label = crate::machines::self_label();
    let mut params = params.clone();
    // 학생이 도는 기계(= 이 기계)는 서버가 채운다 — CLI 의 hostname 짐작보다 정확하다.
    let host_id = params["host"]["machine_id"].as_str().unwrap_or("").trim().to_string();
    if host_id.is_empty() {
        params["host"] = json!({"machine_id": local, "label": self_label});
    }
    let target = params.get("machine_id").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    match target {
        Some(m) if m != local && m != self_label => {
            ensure!(params["local_only"] != true, "nacho report must terminate at nacho's machine ({m}); this machine is not it");
            remote(&m, &params)
        }
        _ => nacho_inbox::submit_local(&params),
    }
}

/// 라벨이든 id 든 명부에서 그 기계를 찾는다. 직통(known_route) → 명부 base → 관문 우회.
fn route(target: &str) -> Result<(Option<String>, String)> {
    let listed = crate::machines::machines().into_iter()
        .find(|m| m.machine_id.as_deref() == Some(target) || m.label == target);
    let machine_id = listed.as_ref().and_then(|m| m.machine_id.clone())
        .or_else(|| listed.is_none().then(|| target.to_string()));
    let base = machine_id.as_deref().and_then(crate::board_service::known_route)
        .or_else(|| listed.as_ref().map(|m| m.base.clone()).filter(|b| !b.is_empty()))
        .or_else(|| machine_id.as_deref().and_then(crate::board_service::relay_base))
        .with_context(|| format!("nacho's machine \"{target}\" has no known route (machines.json / relay)"))?;
    Ok((machine_id, base))
}

fn remote(target: &str, params: &Value) -> Result<Value> {
    let (expected_id, base) = route(target)?;
    let mut body = params.clone();
    body["local_only"] = json!(true);
    body["machine_id"] = json!(expected_id.clone().unwrap_or_else(|| target.to_string()));
    let fingerprint_hint = nacho_inbox::build(params).ok().and_then(|e| e["fingerprint"].as_str().map(str::to_string));
    std::thread::spawn(move || -> Result<Value> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(6))
                .redirect(reqwest::redirect::Policy::none()).build()?;
            let token = crate::remote::connection_auth_token(&base);
            request_remote(&client, &base, token.as_deref(), expected_id.as_deref(), &body, fingerprint_hint.as_deref()).await
        })
    }).join().map_err(|_| anyhow::anyhow!("remote nacho report worker failed; the report was not confirmed"))?
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Value> {
    response = response.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= kasa_socket::board::MAX_SOURCE_BYTES, "remote response is too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// 먼저 그 기계의 `/collab/board?scope=local` 로 신원(machine_id·online)을 확인하고 나서
/// 보고를 보낸다 — 터널 포트가 다른 기계로 바뀌어 있어도 엉뚱한 인박스에 안 들어간다.
async fn request_remote(client: &reqwest::Client, base: &str, token: Option<&str>, expected_id: Option<&str>, body: &Value, fingerprint: Option<&str>) -> Result<Value> {
    let base = base.trim_end_matches('/');
    let with_token = |r: reqwest::RequestBuilder| if let Some(t) = token { r.header("x-kasa-token", t) } else { r };
    let response = with_token(client.get(format!("{base}/collab/board?scope=local"))).send().await.context("remote identity check failed")?;
    if response.status() == reqwest::StatusCode::FORBIDDEN { bail!("remote authentication rejected"); }
    let snapshot = bounded_json(response).await?;
    let sources = snapshot["sources"].as_array().context("remote board has no sources")?;
    let online = sources.iter().find(|s| s["state"] == "online" && s["is_local"] != false);
    let Some(source) = online else { bail!("remote machine is not online at that route") };
    if let Some(id) = expected_id {
        ensure!(source["machine_id"] == id, "remote route identity mismatch: expected {id}");
    }
    // 어느 길로 왔든 다음 홉은 없다 — 받는 쪽이 또 넘기면 두 기계 인박스에 두 통이 남는다.
    let mut forwarded = body.clone();
    forwarded["local_only"] = json!(true);
    let response = with_token(client.post(format!("{base}/nacho/report")).json(&forwarded)).send().await
        .map_err(|_| anyhow::anyhow!("remote result unknown; the report may not have landed — check nacho's inbox before re-sending"))?;
    if response.status() == reqwest::StatusCode::FORBIDDEN { bail!("remote authentication rejected"); }
    let mut value = bounded_json(response).await.context("remote receipt unconfirmed")?;
    if value["ok"] == false { bail!("{}", value["error"].as_str().unwrap_or("remote nacho report failed")); }
    if let Some(fp) = fingerprint { ensure!(value["fingerprint"] == fp, "remote receipt fingerprint mismatch"); }
    ensure!(matches!(value["state"].as_str(), Some("accepted" | "duplicate")), "remote receipt state unconfirmed");
    value["via"] = json!("remote");
    value["machine"] = json!(source["label"].as_str().unwrap_or(""));
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};

    fn report() -> Value {
        json!({"origin":"nacho","conv":"discord:1","task_id":"t-1","surface":"%2","cwd":"/repo",
               "status":"done","summary":"끝","changed":["x.rs"],"tests":"cargo test ok","next":""})
    }

    async fn fake_nacho_machine(online: bool, wrong_id: bool, deny: bool) -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Option<Value>>>, tokio::task::JoinHandle<()>) {
        let posts = Arc::new(AtomicUsize::new(0));
        let got = Arc::new(std::sync::Mutex::new(None));
        let (posts_c, got_c) = (posts.clone(), got.clone());
        let id = if wrong_id { "someone-else" } else { "nacho-machine" };
        let board = json!({"scope":"local","sources":[{"machine_id":id,"label":"미니","state":if online {"online"} else {"stale"},"is_local":true}],"panes":[]});
        let app = axum::Router::new()
            .route("/collab/board", axum::routing::get(move |headers: axum::http::HeaderMap| {
                let board = board.clone();
                async move {
                    if deny || headers.get("x-kasa-token").and_then(|v| v.to_str().ok()) != Some("fake-token") {
                        (axum::http::StatusCode::FORBIDDEN, axum::Json(json!({})))
                    } else { (axum::http::StatusCode::OK, axum::Json(board)) }
                }
            }))
            .route("/nacho/report", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                posts_c.fetch_add(1, Ordering::SeqCst);
                assert_eq!(body["local_only"], true, "forwarded reports must terminate at nacho's machine");
                let envelope = nacho_inbox::build(&body).unwrap();
                *got_c.lock().unwrap() = Some(envelope.clone());
                async move { axum::Json(json!({"ok":true,"report_id":envelope["report_id"],"fingerprint":envelope["fingerprint"],"state":"accepted","wake":"queued"})) }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (base, posts, got, server)
    }

    #[tokio::test]
    async fn remote_report_checks_identity_before_posting() {
        let client = reqwest::Client::new();
        for (online, wrong_id, deny) in [(false, false, false), (true, true, false), (true, false, true)] {
            let (base, posts, _got, server) = fake_nacho_machine(online, wrong_id, deny).await;
            let err = request_remote(&client, &base, Some("fake-token"), Some("nacho-machine"), &report(), None).await.unwrap_err().to_string();
            assert!(err.contains("online") || err.contains("mismatch") || err.contains("rejected"), "{err}");
            assert_eq!(posts.load(Ordering::SeqCst), 0, "never post when identity is unconfirmed");
            server.abort();
        }
    }

    #[tokio::test]
    async fn remote_report_lands_with_local_only_and_host_preserved() {
        let (base, posts, got, server) = fake_nacho_machine(true, false, false).await;
        let mut body = report();
        body["host"] = json!({"machine_id":"student-machine","label":"맥북"});
        let fp = nacho_inbox::build(&body).unwrap()["fingerprint"].as_str().unwrap().to_string();
        let receipt = request_remote(&reqwest::Client::new(), &base, Some("fake-token"), Some("nacho-machine"), &body, Some(&fp)).await.unwrap();
        assert_eq!(receipt["state"], "accepted");
        assert_eq!(receipt["via"], "remote");
        assert_eq!(receipt["machine"], "미니");
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        let stored = got.lock().unwrap().clone().unwrap();
        assert_eq!(stored["host"]["label"], "맥북", "the student's machine survives the hop");
        assert_eq!(stored["fingerprint"], fp);
        server.abort();
    }

    #[test]
    fn submit_local_only_never_forwards() {
        let mut body = report();
        body["machine_id"] = json!("some-other-machine-id");
        body["local_only"] = json!(true);
        let err = submit(&body).unwrap_err().to_string();
        assert!(err.contains("terminate"), "{err}");
    }
}
