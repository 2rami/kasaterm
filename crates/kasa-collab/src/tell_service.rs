//! Transport routing ends at the receiving machine; only that machine owns receipts.
use anyhow::{bail, ensure, Context, Result};
use kasa_socket::{Backend, tell::{self, Address, Ledger, Record, State}};
use serde_json::{json, Value};
use std::sync::{Mutex, OnceLock};

static STORAGE_ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();

/// 장부를 둘 뿌리를 호스트가 정한다(첫 쓰기 전에 한 번). 장부는 한 프로세스만 쥘 수 있어,
/// 같은 기계에서 본판과 함께 도는 `kasa tui` 서버는 자기 폴더를 쓴다. 안 정하면 본판 자리다.
pub fn set_storage_root(root: std::path::PathBuf) -> bool {
    STORAGE_ROOT.set(root).is_ok()
}

fn ledger() -> Result<&'static Mutex<Ledger>> {
    static LEDGER: OnceLock<Result<Mutex<Ledger>, String>> = OnceLock::new();
    LEDGER.get_or_init(|| {
        let root = if let Some(root) = STORAGE_ROOT.get().cloned() { root }
        else if let Some(root) = kasa_socket::isolated_collab_root() { root }
        else if let Some(path) = std::env::var_os("KASATERM_SESSION_FILE") {
            std::path::PathBuf::from(path).parent().map(std::path::Path::to_path_buf)
                .ok_or("session storage parent unavailable")?
        } else { kasa_socket::home_dir().ok_or("home directory unavailable")?.join(".config/kasaterm") };
        Ledger::open(root.join("tell-receipts").join("ledger.json")).map(Mutex::new).map_err(|e|e.to_string())
    }).as_ref().map_err(|e|anyhow::anyhow!("{e}"))
}
pub fn pending() -> Result<Vec<Record>> {
    let mut ledger = ledger()?.lock().map_err(|_|anyhow::anyhow!("tell ledger lock failed"))?;
    ledger.expire_pending(tell::now_ms())?;
    Ok(ledger.pending())
}
pub fn transition(id: &str, state: State, reason: &str) -> Result<Record> {
    ledger()?.lock().map_err(|_|anyhow::anyhow!("tell ledger lock failed"))?.transition(id,state,reason)
}

pub fn current_address(backend: &dyn Backend, surface: &str) -> Result<Address> {
    let live = kasa_pty::lookup_session(surface).context("target is not a live local PTY")?;
    ensure!(!live.input_closed(), "target pane is closed");
    ensure!(!crate::env::env().is_remote_pane(surface), "use the remote pane's global address");
    ensure!(matches!(live.active_agent(),Some(kasa_pty::AgentKind::Claude | kasa_pty::AgentKind::Codex)), "shell or unsupported harness cannot receive tell");
    let value = backend.collab_tell_identity(surface)?;
    Address::parse(&value)
}

pub fn submit(backend: &dyn Backend, params: &Value, wake: impl FnOnce() -> Result<()>) -> Result<Value> {
    ensure!(params.get("force").is_none_or(|v|v == false), "force cannot bypass tell protection");
    let address = match params.get("address") {
        Some(value) => Address::parse(value)?,
        None => current_address(backend,params["surface_id"].as_str().context("tell requires address or local surface_id")?)?,
    };
    let id = params["message_id"].as_str().context("tell requires message_id")?;
    tell::valid_id(id)?;
    let body = tell::normalize(params["body"].as_str().context("tell requires body")?)?;
    if let Some(flag) = tell::leaked_cli_flag(&body) {
        bail!("본문이 CLI 옵션 {flag} 로 시작해요 — 보낸 기기의 kasaterm-cli 가 그 옵션을 모르는 옛 판이면 옵션이 본문이 되고 진짜 본문은 빠져요. 그 기기 앱을 업데이트하세요");
    }
    let mut normalized = params.clone(); normalized["address"] = serde_json::to_value(&address)?;
    normalized["body"] = json!(body);
    if address.machine_id != crate::board_service::local_id()? {
        ensure!(params["local_only"] != true,"tell must terminate at its receiving machine");
        return remote(&address,&normalized,"/collab/tell");
    }
    {
        let store = ledger()?.lock().map_err(|_|anyhow::anyhow!("tell ledger lock failed"))?;
        if let Some(existing) = store.existing(id,&address,&body)? { return Ok(existing.receipt()); }
    }
    ensure!(current_address(backend,&address.surface_id)? == address, "target identity changed; refresh the board");
    // Reject policy is evaluated on the GUI thread, alongside IME and draft state.
    ensure!(matches!(params["policy"].as_str().unwrap_or("queue"),"queue"|"reject"), "policy must be queue or reject");
    let identity = backend.collab_tell_identity(&address.surface_id)?;
    ensure!(Address::parse(&identity)? == address,"target changed while accepting message");
    let receiver_pid = identity["agent_pid"].as_u64().context("current agent process evidence unavailable")? as u32;
    let record = ledger()?.lock().map_err(|_|anyhow::anyhow!("tell ledger lock failed"))?
        .accept_with_policy(id,address,body,params["ttl_seconds"].as_u64().unwrap_or(tell::QUEUE_TTL_SECONDS),params["policy"] == "reject",receiver_pid,
            params["title"].as_str().unwrap_or("").to_owned())?;
    if wake().is_err() {
        return Ok(transition(id,State::Failed,"GUI delivery event was not accepted")?.receipt());
    }
    Ok(record.receipt())
}

pub fn status(params: &Value) -> Result<Value> {
    let address = Address::parse(&params["address"])?;
    if address.machine_id != crate::board_service::local_id()? {
        ensure!(params["local_only"] != true,"receipt lookup must terminate at its receiving machine");
        return remote(&address,params,"/collab/tell/status");
    }
    let id = params["message_id"].as_str().context("status requires message_id")?;
    Ok(ledger()?.lock().map_err(|_|anyhow::anyhow!("tell ledger lock failed"))?.status(id,&address)?.receipt())
}

#[cfg(not(feature = "net"))]
fn remote(_address: &Address, _params: &Value, _path: &'static str) -> Result<Value> {
    bail!("다른 기계로 보내려면 kasa-collab feature net 이 필요하다")
}

#[cfg(feature = "net")]
fn remote(address: &Address, params: &Value, path: &'static str) -> Result<Value> {
    // 직통(검증된 길) → 명부의 base → 관문 우회. 우회는 넷버드·터널 없이도 닿는다.
    let base = crate::board_service::known_route(&address.machine_id)
        .or_else(||crate::env::env().machines().into_iter()
            .find(|m|m.machine_id.as_deref() == Some(address.machine_id.as_str())).map(|m|m.base))
        .or_else(||crate::board_service::relay_base(&address.machine_id))
        .context("remote machine identity has no verified known route")?;
    let expected = address.clone();
    let mut body = params.clone(); body["local_only"] = json!(true);
    std::thread::spawn(move || -> Result<Value> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none()).build()?;
            let token = crate::env::env().auth_token(&base);
            request_remote(&client,&base,token.as_deref(),&expected,&body,path).await
        })
    }).join().map_err(|_|anyhow::anyhow!("remote tell worker failed; inspect status before retrying"))?
}

#[cfg(feature = "net")]
async fn bounded_json(mut response: reqwest::Response) -> Result<Value> {
    response = response.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(bytes.len()+chunk.len() <= kasa_socket::board::MAX_SOURCE_BYTES,"remote tell response is too large");
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(feature = "net")]
async fn request_remote(client: &reqwest::Client, base: &str, token: Option<&str>, expected: &Address, body: &Value, path: &str) -> Result<Value> {
    let request = client.get(format!("{}/collab/board?scope=local",base.trim_end_matches('/')));
    let request = if let Some(token) = token { request.header("x-kasa-token",token) } else { request };
    let response = request.send().await.context("remote identity check failed")?;
    if response.status() == reqwest::StatusCode::FORBIDDEN { bail!("remote authentication rejected"); }
    let snapshot = bounded_json(response).await?;
    ensure!(snapshot["scope"] == "local" && snapshot["sources"].as_array().is_some_and(|sources|sources.iter().any(|s|s["machine_id"] == expected.machine_id && s["state"] == "online")), "remote route identity mismatch");
    if path == "/collab/tell" {
        ensure!(snapshot["panes"].as_array().is_some_and(|panes|panes.iter().any(|p|Address::parse(&p["address"]).ok().as_ref() == Some(expected))), "remote pane identity changed");
    }
    let mut forwarded = body.clone(); forwarded["local_only"] = json!(true);
    let request = client.post(format!("{}{path}",base.trim_end_matches('/'))).json(&forwarded);
    let request = if let Some(token) = token { request.header("x-kasa-token",token) } else { request };
    let response = request.send().await.map_err(|_|anyhow::anyhow!("remote result unknown; query tell-status with the same message_id and address; do not issue a new ID"))?;
    if response.status() == reqwest::StatusCode::FORBIDDEN { bail!("remote authentication rejected"); }
    let value = bounded_json(response).await.context("remote receipt unconfirmed; inspect the same ID before retrying")?;
    if value["ok"] == false { bail!("{}",value["error"].as_str().unwrap_or("remote tell failed")); }
    ensure!(Address::parse(&value["address"])? == *expected,"remote receipt identity mismatch");
    ensure!(value["message_id"] == body["message_id"],"remote receipt message ID mismatch");
    if let Some(text) = body["body"].as_str() { ensure!(value["body_hash"] == tell::fingerprint(text),"remote receipt body mismatch"); }
    ensure!(serde_json::from_value::<State>(value["state"].clone()).is_ok(),"remote receipt state unconfirmed");
    Ok(value)
}

/// 시험용 가짜 원격 기계(`/collab/board`·`/collab/tell`). 다른 크레이트 시험도 쓴다(feature `test-support`).
#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::*;
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    pub fn fixture_address() -> Address { Address {machine_id:"fixture-remote-machine".into(),surface_key:"key".into(),surface_id:"%7".into(),session_id:"session".into(),instance_id:"instance".into()} }
    pub async fn fake_server(deny: bool, changed: bool, disconnect: bool) -> (String,Arc<AtomicUsize>,tokio::task::JoinHandle<()>) {
        fake_server_auth(deny,changed,disconnect,true).await
    }
    pub async fn fake_server_auth(deny: bool, changed: bool, disconnect: bool, require_token: bool) -> (String,Arc<AtomicUsize>,tokio::task::JoinHandle<()>) {
        use axum::response::IntoResponse;
        let count = Arc::new(AtomicUsize::new(0));
        let calls = count.clone();
        let mut target = fixture_address(); if changed { target.session_id = "replacement".into(); }
        let source = json!({"scope":"local","sources":[{"machine_id":"fixture-remote-machine","state":"online"}],"panes":[{"address":target}]});
        let app = axum::Router::new()
            .route("/collab/board",axum::routing::get(move |headers: axum::http::HeaderMap| {
                let source = source.clone();
                async move {
                    if deny || (require_token && headers.get("x-kasa-token").and_then(|v|v.to_str().ok()) != Some("fake-token")) {
                        (axum::http::StatusCode::FORBIDDEN,axum::Json(json!({})))
                    } else { (axum::http::StatusCode::OK,axum::Json(source)) }
                }
            }))
            .route("/collab/tell",axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                assert_eq!(body["local_only"],true,"every outgoing hop must terminate at its addressed receiver");
                calls.fetch_add(1,Ordering::SeqCst);
                async move {
                    if disconnect {
                        let stream = futures_util::stream::once(async { Err::<bytes::Bytes,_>(std::io::Error::new(std::io::ErrorKind::ConnectionReset,"fake disconnect after dispatch")) });
                        return axum::response::Response::new(axum::body::Body::from_stream(stream));
                    }
                    axum::Json(json!({"message_id":body["message_id"],"address":body["address"],"state":"accepted","body_hash":tell::fingerprint(body["body"].as_str().unwrap())})).into_response()
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}",listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        (base,count,server)
    }
}

#[cfg(all(test, feature = "net"))]
mod tests {
    use super::*;
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    use super::testing::{fake_server, fixture_address as address};
    #[tokio::test]
    async fn fake_http_authentication_and_identity_rejection_never_posts() {
        for (deny,changed) in [(true,false),(false,true)] {
            let (base,calls,server) = fake_server(deny,changed,false).await;
            let body = json!({"message_id":tell::new_message_id(),"address":address(),"body":"hello"});
            assert!(request_remote(&reqwest::Client::new(),&base,Some("fake-token"),&address(),&body,"/collab/tell").await.is_err());
            assert_eq!(calls.load(Ordering::SeqCst),0);
            server.abort();
        }
    }
    #[tokio::test]
    async fn fake_http_lost_receipt_never_retries_post() {
        let (base,calls,server) = fake_server(false,false,true).await;
        let body = json!({"message_id":tell::new_message_id(),"address":address(),"body":"hello"});
        assert!(request_remote(&reqwest::Client::new(),&base,Some("fake-token"),&address(),&body,"/collab/tell").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),1);
        server.abort();
    }
    #[tokio::test]
    async fn fake_http_returns_receiver_receipt_without_read_claim() {
        let (base,calls,server) = fake_server(false,false,false).await;
        let body = json!({"message_id":tell::new_message_id(),"address":address(),"body":"hello"});
        let receipt = request_remote(&reqwest::Client::new(),&base,Some("fake-token"),&address(),&body,"/collab/tell").await.unwrap();
        assert_eq!(receipt["state"],"accepted");
        assert!(receipt.get("read").is_none());
        assert_eq!(calls.load(Ordering::SeqCst),1);
        server.abort();
    }

}
