//! 칸·방에 쓰는 일 — raw 입력·칸 세우기·옮기기·닫기·이름·색·포커스·거울 탭. 주소의 기기가 이 기기가 아니면
//! tell 과 같은 길(직통 → 명부 base → 관문 우회)·같은 인증으로 그 기기에 넘기고, 그 기기가 보드 주소(surface_key·
//! 신원)를 확인한 뒤 제 칸에만 한다. tell 의 안전장치(빈 입력창 기다리기·영수증)는 없다 — 셸·승인 화면용이다.
use anyhow::{bail, ensure, Context, Result};
use kasa_socket::{backend::Backend, board::field, protocol::Request};
use serde_json::{json, Value};

/// 원본이 받는 raw 글 상한. HTTP 몸통 상한(`/collab/act` 64 KiB) 안에서 주소·옵션 자리를 남긴다.
pub const TEXT_MAX: usize = 48 * 1024;

pub fn act(backend: &dyn Backend, params: &Value) -> Result<Value> {
    let op = params["op"].as_str().filter(|o| !o.is_empty()).context("act requires op")?;
    let address = params.get("address").filter(|a| a.is_object());
    let local = crate::board_service::local_id()?;
    let machine = match address {
        Some(a) => field(a, "machine_id").context("address requires machine_id")?.to_owned(),
        None => params["machine_id"].as_str().filter(|m| !m.is_empty()).unwrap_or(&local).to_owned(),
    };
    if machine != local {
        ensure!(params["local_only"] != true, "remote act must terminate at its source");
        return remote(&machine, op, params);
    }
    let mut out = run_local(backend, op, address, params)?;
    out["ok"] = json!(true);
    out["op"] = json!(op);
    out["machine_id"] = json!(local);
    Ok(out)
}

fn run_local(backend: &dyn Backend, op: &str, address: Option<&Value>, params: &Value) -> Result<Value> {
    let pane = || -> Result<String> { place(backend, address.with_context(|| format!("{op} requires address"))?) };
    let text = |key: &str| -> Result<&str> {
        params[key].as_str().filter(|s| !s.is_empty()).with_context(|| format!("{op} requires {key}"))
    };
    let (method, args) = match op {
        "send_text" => {
            let surface = writable(pane()?)?;
            let body = text("text")?;
            ensure!(body.len() <= TEXT_MAX, "text is too long ({} bytes, max {TEXT_MAX})", body.len());
            ("surface.send_text", json!({"surface_id": surface, "text": body}))
        }
        "send_key" => ("surface.send_key", json!({"surface_id": writable(pane()?)?, "key": text("key")?})),
        "persona" => ("surface.repersona", json!({"surface_id": writable(pane()?)?, "character": text("character")?})),
        "focus" => ("surface.focus", json!({"surface_id": pane()?})),
        "rename" => ("surface.rename", json!({"surface_id": pane()?, "title": text("title")?})),
        "color" => ("surface.set_color", json!({"surface_id": pane()?, "color": text("color")?})),
        "rename_room" => ("window.rename", json!({"surface_id": pane()?, "title": text("title")?})),
        "move" => {
            let target = params.get("target").filter(|t| t.is_object()).context("move requires target address")?;
            ensure!(field(target, "machine_id") == address.and_then(|a| field(a, "machine_id")),
                "move target must be on the same machine");
            let direction = params["direction"].as_str().unwrap_or("right");
            ("surface.move", json!({"surface_id": pane()?, "target": place(backend, target)?, "direction": direction}))
        }
        "close" => return close(backend, &pane()?, params["force"] == true),
        "spawn" => return spawn(backend, address, params),
        "attach" => return attach(backend, address, params),
        _ => bail!("unknown act {op:?}"),
    };
    let reply = kasa_socket::methods::dispatch(backend, Request { id: json!(op), method: method.into(), params: args });
    if !reply.ok {
        bail!("{}", reply.error.map(|e| e.message).unwrap_or_else(|| format!("{method} failed")));
    }
    Ok(json!({"address": address}))
}

/// 주소가 가리키는 이 기기 칸. 번호가 다시 짜였거나 세션·프로세스가 바뀐 낡은 주소는 거절한다.
fn place(backend: &dyn Backend, address: &Value) -> Result<String> {
    let surface = field(address, "surface_id").context("address requires surface_id")?;
    let key = field(address, "surface_key").context("address requires surface_key")?;
    ensure!(crate::surface_keys::get(surface).as_deref() == Some(key), "surface identity changed; refresh the board");
    crate::board_service::validate_address(&backend.collab_pane_identity(surface)?, address)?;
    Ok(surface.to_owned())
}

/// 거울 칸에 넣은 글은 원본 기기로 흘러간다 — 받는 칸을 원본 주소로 고르게 한다(tell 과 같다).
fn writable(surface: String) -> Result<String> {
    ensure!(!crate::env::env().is_remote_pane(&surface), "mirror pane; use the source machine's address");
    Ok(surface)
}

/// 닫기 전에 그 칸 폴더의 커밋 안 된 변경을 센다 — CLI `close` 와 같은 보호를 칸이 사는 기기에서.
fn close(backend: &dyn Backend, surface: &str, force: bool) -> Result<Value> {
    let cwd = backend.pane_cwds().into_iter().find(|(id, _)| id == surface).map(|(_, cwd)| cwd).unwrap_or_default();
    let dirty = if force || cwd.is_empty() { 0 } else { kasa_socket::cli::git_dirty_count(&cwd) };
    if dirty > 0 {
        return Ok(json!({"closed": false, "dirty": dirty, "cwd": cwd}));
    }
    let reply = kasa_socket::methods::dispatch(backend, Request {
        id: json!("close"), method: "surface.close".into(), params: json!({"surface_id": surface}),
    });
    if !reply.ok {
        bail!("{}", reply.error.map(|e| e.message).unwrap_or_else(|| "close failed".into()));
    }
    Ok(json!({"closed": true, "cwd": cwd}))
}

/// 이 기기 방에 맨 셸 칸을 세운다 — `address` 옆(`tab` 이면 그 칸의 탭), `window:"new"` 면 새 방, 둘 다 없으면 보던 방.
/// 돌려주는 것은 새 칸의 보드 주소다(아직 세션 없음).
fn spawn(backend: &dyn Backend, address: Option<&Value>, params: &Value) -> Result<Value> {
    use kasa_socket::backend::{SpawnShellAt, SpawnWindow};
    let cwd = match params["cwd"].as_str().filter(|c| !c.is_empty()) {
        Some(dir) => {
            let path = std::path::Path::new(dir);
            ensure!(path.is_absolute() && path.is_dir(), "no such folder on this machine: {dir}");
            Some(path.canonicalize()?.to_string_lossy().into_owned())
        }
        None => None,
    };
    let mut at = SpawnShellAt { cwd: cwd.clone(), ..Default::default() };
    if params["window"] == "new" {
        at.window = Some(SpawnWindow::New);
    } else if let Some(anchor) = address {
        let surface = place(backend, anchor)?;
        if params["tab"] == true { at.tab_of = Some(surface) } else { at.beside = Some(surface) }
    }
    let reply = backend.spawn_shell_at(&at)?;
    if reply.surface.is_empty() {
        bail!("{}", reply.error.unwrap_or_else(|| "could not place a pane".into()));
    }
    Ok(json!({"address": crate::board_service::address(&reply.surface, None)?, "window": reply.window, "cwd": cwd}))
}

/// 다른 기기 칸(`source`)을 이 기기에 거울 탭으로 연다 — `address` 가 있으면 그 칸의 탭, 없으면 보던 칸의 탭.
/// 사람이 그 기기 화면으로 다른 기기 학생의 승인·질문에 답하는 길이다.
fn attach(backend: &dyn Backend, address: Option<&Value>, params: &Value) -> Result<Value> {
    let source = params.get("source").filter(|s| s.is_object()).context("attach requires source address")?;
    let machine = field(source, "machine_id").context("source requires machine_id")?;
    let remote_id = field(source, "surface_id").context("source requires surface_id")?;
    ensure!(machine != crate::board_service::local_id()?, "source pane is on this machine; focus it instead");
    let board = crate::board_service::snapshot(&json!({"scope": "all"}))?;
    let row = board["panes"].as_array().into_iter().flatten()
        .find(|p| crate::board_service::validate_address(&p["address"], source).is_ok())
        .context("source pane is not on the current board; refresh the board")?;
    let label = crate::board_service::roster_label(machine)
        .context("source machine has no direct roster route on this machine")?;
    let anchor = address.map(|a| place(backend, a)).transpose()?;
    let name = row["character"].as_str().or(row["title"].as_str()).unwrap_or("");
    let cwd = row["cwd"].as_str().unwrap_or("");
    let surface = backend.mirror_pane(&label, remote_id, name, cwd, anchor.as_deref(), params["focus"] == true)?;
    Ok(json!({"address": crate::board_service::address(&surface, None)?, "source": source}))
}

#[cfg(not(feature = "net"))]
fn remote(_machine: &str, _op: &str, _params: &Value) -> Result<Value> {
    bail!("다른 기계 칸에 쓰려면 kasa-collab feature net 이 필요하다")
}

#[cfg(feature = "net")]
fn remote(machine: &str, op: &str, params: &Value) -> Result<Value> {
    let base = crate::board_service::remote_base(machine).context("remote machine has no current route")?;
    let mut body = params.clone();
    body["local_only"] = json!(true);
    let (machine, op) = (machine.to_owned(), op.to_owned());
    std::thread::spawn(move || -> Result<Value> {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
            // 원본 앱은 칸 세우기·옮기기·닫기에 GUI 답을 20초까지 기다린다.
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none()).build()?;
            let token = crate::env::env().auth_token(&base);
            request_remote(&client, &base, token.as_deref(), &machine, &op, &body).await
        })
    }).join().map_err(|_| anyhow::anyhow!("remote act worker failed; check the pane before retrying"))?
}

/// tell 과 같은 두 걸음: 그 base 가 지금 그 기기인지(인증 포함) 판으로 확인한 뒤 보낸다. 칸 신원은 받는 기기가
/// 제 칸에서 확인한다 — 갓 세운 칸은 판에 아직 없을 수 있다.
#[cfg(feature = "net")]
async fn request_remote(client: &reqwest::Client, base: &str, token: Option<&str>, machine: &str, op: &str, body: &Value) -> Result<Value> {
    let base = base.trim_end_matches('/');
    let request = client.get(format!("{base}/collab/board?scope=local"));
    let request = if let Some(token) = token { request.header("x-kasa-token", token) } else { request };
    let response = request.send().await.context("remote identity check failed")?;
    if response.status() == reqwest::StatusCode::FORBIDDEN { bail!("remote authentication rejected"); }
    let snapshot = crate::tell_service::bounded_json(response).await?;
    ensure!(snapshot["scope"] == "local" && snapshot["sources"].as_array().is_some_and(|sources|
        sources.iter().any(|s| s["machine_id"] == machine && s["state"] == "online")), "remote route identity mismatch");
    let request = client.post(format!("{base}/collab/act")).json(body);
    let request = if let Some(token) = token { request.header("x-kasa-token", token) } else { request };
    let response = request.send().await
        .map_err(|_| anyhow::anyhow!("remote result unknown; peek the pane before retrying"))?;
    match response.status().as_u16() {
        403 => bail!("remote authentication rejected"),
        404 | 405 => bail!("unsupported_api: that machine's kasaterm is too old for {op}"),
        _ => {}
    }
    let value = crate::tell_service::bounded_json(response).await.context("remote result unconfirmed; peek the pane before retrying")?;
    if value["ok"] != true {
        bail!("{}", value["error"].as_str().unwrap_or("remote act failed"));
    }
    ensure!(value["machine_id"] == machine && value["op"] == op, "remote act identity mismatch");
    Ok(value)
}

#[cfg(all(test, feature = "net"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    async fn fake(identity: &'static str, status: u16) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let app = axum::Router::new()
            .route("/collab/board", axum::routing::get(move || async move {
                axum::Json(json!({"scope":"local","sources":[{"machine_id":identity,"state":"online"}],"panes":[]}))
            }))
            .route("/collab/act", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                recorder.lock().unwrap().push(body.clone());
                async move {
                    let code = axum::http::StatusCode::from_u16(status).unwrap();
                    (code, axum::Json(json!({"ok":true,"op":body["op"],"machine_id":"far","address":body["address"]})))
                }
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        (base, seen, tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); }))
    }

    /// 넘기는 몸통은 받는 기기에서 끝나야 하고(`local_only`), 답은 물은 기기·물은 일의 것이어야 한다.
    #[tokio::test]
    async fn remote_act_terminates_at_its_source_and_checks_the_reply() {
        let (base, seen, server) = fake("far", 200).await;
        let body = json!({"op":"send_text","address":{"machine_id":"far"},"text":"ls\n","local_only":true});
        let value = request_remote(&reqwest::Client::new(), &base, None, "far", "send_text", &body).await.unwrap();
        assert_eq!(value["op"], "send_text");
        assert_eq!(seen.lock().unwrap()[0]["local_only"], true);
        assert!(request_remote(&reqwest::Client::new(), &base, None, "far", "close", &body).await.is_err(),
            "다른 일의 답은 믿지 않는다");
        server.abort();
    }

    /// 그 base 에 다른 기기가 앉아 있으면 보내지 않는다. 옛 판(404)은 「옛 판」으로 말한다.
    #[tokio::test]
    async fn remote_act_refuses_wrong_machine_and_names_old_versions() {
        let (base, seen, server) = fake("someone-else", 200).await;
        let body = json!({"op":"focus"});
        assert!(request_remote(&reqwest::Client::new(), &base, None, "far", "focus", &body).await.is_err());
        assert!(seen.lock().unwrap().is_empty(), "신원이 다르면 POST 가 없어야 한다");
        server.abort();
        let (base, _, server) = fake("far", 404).await;
        let error = request_remote(&reqwest::Client::new(), &base, None, "far", "focus", &body).await.unwrap_err();
        assert!(error.to_string().contains("unsupported_api"), "{error}");
        server.abort();
    }
}
