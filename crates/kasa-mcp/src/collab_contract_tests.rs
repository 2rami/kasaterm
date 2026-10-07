//! 협업 호스트(kasa-collab)와 이 크레이트의 HTTP·독립 백엔드·본판 원천이 맞물리는지 보는 계약 시험.
//! kasa-collab 혼자서는 이 크레이트를 모르니 여기 둔다.

use kasa_collab::tell_service::testing::{fake_server_auth, fixture_address as address};
use kasa_socket::{tell, Backend};
use serde_json::{json, Value};
use std::sync::{atomic::Ordering, Arc};

#[test]
fn production_source_paths_do_not_reenter_legacy_agent_or_peer_inventory() {
    let desktop = include_str!("../../../app/kasaterm/src/socket.rs");
    let source = desktop
        .split("fn collab_board_source(&self)")
        .nth(1)
        .unwrap()
        .split("fn collab_board(&self)")
        .next()
        .unwrap();
    for forbidden in [
        "agents_status(",
        "agents_cached(",
        "rebind_agents_panes(",
        "peers::",
    ] {
        assert!(!source.contains(forbidden), "new source called {forbidden}");
    }
    let standalone = include_str!("standalone.rs");
    let source = standalone
        .split("fn collab_board_source(&self)")
        .nth(1)
        .unwrap()
        .split("// --- required")
        .next()
        .unwrap();
    assert!(
        !source.contains("claude_bin")
            && !source.contains("Command")
            && !source.contains("collab_board(")
    );
    assert!(
        source.contains("kasa_pty::live_sessions()") && source.contains("activity_unsupported")
    );
}
#[test]
#[ignore = "isolated subprocess invoked by standalone relay contract test"]
fn standalone_relay_child() {
    let Some(params) = std::env::var("KASATERM_TEST_TELL_PARAMS").ok() else { return };
    crate::install_collab_env();
    assert_eq!(std::env::var("KASATERM_MACHINE_ID").unwrap(),"fixture-local-machine");
    let params: Value = serde_json::from_str(&params).unwrap();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
        let backend: Arc<dyn Backend> = Arc::new(crate::standalone::StandaloneBackend::new(std::env::temp_dir()));
        let app = axum::Router::new().route("/collab/tell",axum::routing::post(move |body: axum::Json<Value>| {
            crate::http::collab_tell_post(backend.clone(),body)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/collab/tell",listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener,app).await.unwrap(); });
        let receipt: Value = reqwest::Client::new().post(url).json(&params).send().await.unwrap().json().await.unwrap();
        if params["local_only"] == true {
            assert!(receipt["error"].as_str().unwrap().contains("terminate"));
        } else if std::env::var("KASATERM_TEST_TELL_DENY").as_deref() == Ok("1") {
            assert!(receipt["error"].as_str().unwrap().contains("authentication rejected"));
        } else {
            assert_eq!(receipt["state"],"accepted");
            assert_eq!(receipt["address"],params["address"]);
            assert!(receipt.get("read").is_none());
        }
        server.abort();
    });
}

#[tokio::test]
async fn standalone_backend_relays_remote_tell_and_preserves_auth_rejection() {
    for (deny,local_only) in [(false,false),(true,false),(false,true)] {
        let (base,calls,server) = fake_server_auth(deny,false,false,false).await;
        let params = json!({"message_id":tell::new_message_id(),"address":address(),"body":"hello from standalone","local_only":local_only});
        let roster = json!([{"label":"fixture-native","machine_id":"fixture-remote-machine","base":base}]);
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","collab_contract_tests::standalone_relay_child","--ignored","--nocapture"])
                .env("KASATERM_MACHINE_ID","fixture-local-machine")
                .env("KASATERM_MACHINES",roster.to_string())
                .env("KASATERM_TEST_TELL_PARAMS",params.to_string())
                .env("KASATERM_TEST_TELL_DENY",if deny {"1"} else {"0"})
                .output().unwrap()
        }).await.unwrap();
        assert!(output.status.success(),"standalone relay child failed: {} {}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        assert_eq!(calls.load(Ordering::SeqCst),if deny || local_only {0} else {1});
        server.abort();
    }
}

fn view_address() -> Value {
    json!({"machine_id":"fixture-remote-machine","surface_key":"key","surface_id":"%7","session_id":"session","instance_id":"instance"})
}

/// 다른 기기 칸의 화면 글·그림과 그 기기 방 배치가 그 기기에 물어 돌아온다. 저쪽에는 「여기서 끝내라」
/// (`local_only`)가 실리고 캡처 경로는 안 실린다 — 그림은 이 기기 파일로 푼다.
#[tokio::test]
async fn remote_views_reach_their_source_and_capture_lands_locally() {
    use axum::extract::{Path, Query};
    use base64::Engine as _;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
    let recorder = seen.clone();
    let app = axum::Router::new().route("/collab/{op}", axum::routing::get(
        move |Path(op): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>| {
            let recorder = recorder.clone();
            async move {
                let params: Value = serde_json::from_str(q.get("params").map(String::as_str).unwrap_or("{}")).unwrap();
                recorder.lock().unwrap().push((op.clone(), params.clone()));
                let png = b"\x89PNG\r\n\x1a\nfixture";
                axum::Json(match op.as_str() {
                    "board" => json!({"schema_version":1,"scope":"local","panes":[],"sources":[]}),
                    "peek" => json!({"address":params["address"],"text":"remote screen","truncated":false}),
                    "where" => json!({"machine_id":"fixture-remote-machine","machine_label":"Fixture","rooms":[]}),
                    "capture" => json!({"address":params["address"],"bytes":png.len(),
                        "png_base64":base64::engine::general_purpose::STANDARD.encode(png)}),
                    _ => json!({}),
                })
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let roster = json!([{"label":"fixture-native","machine_id":"fixture-remote-machine","base":base}]);
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "collab_contract_tests::remote_view_child", "--ignored", "--nocapture"])
            .env("KASATERM_MACHINE_ID", "fixture-local-machine")
            .env("KASATERM_MACHINES", roster.to_string())
            .env("KASATERM_TEST_VIEW", "1")
            .output().unwrap()
    }).await.unwrap();
    assert!(output.status.success(), "remote view child failed: {} {}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let seen = seen.lock().unwrap();
    let ops: Vec<&str> = seen.iter().map(|(op, _)| op.as_str()).filter(|op| *op != "board").collect();
    assert_eq!(ops, ["peek", "where", "capture"]);
    for (op, params) in seen.iter().filter(|(op, _)| op != "board") {
        assert_eq!(params["local_only"], true, "{op} must terminate at its source");
        assert!(params.get("path").is_none(), "{op} must not carry a path to the source");
    }
    server.abort();
}

#[test]
#[ignore = "isolated subprocess invoked by remote view contract test"]
fn remote_view_child() {
    if std::env::var("KASATERM_TEST_VIEW").is_err() { return; }
    crate::install_collab_env();
    let backend = crate::standalone::StandaloneBackend::new(std::env::temp_dir());
    let view = |op: &str, params: Value| kasa_collab::board_service::view(&backend, op, &params);
    let peek = view("peek", json!({"address":view_address(),"lines":12})).unwrap();
    assert_eq!(peek["text"], "remote screen");
    let place = view("where", json!({"machine_id":"fixture-remote-machine"})).unwrap();
    assert_eq!(place["machine_label"], "Fixture");
    let path = std::env::temp_dir().join(format!("kasa-remote-view-{}.png", std::process::id()));
    let shot = view("capture", json!({"address":view_address(),"path":path})).unwrap();
    assert_eq!(shot["remote"], true);
    assert!(shot.get("png_base64").is_none());
    assert!(std::fs::read(&path).unwrap().starts_with(b"\x89PNG"));
    let _ = std::fs::remove_file(&path);
    let echoed = view("peek", json!({"address":view_address(),"local_only":true})).unwrap_err();
    assert!(echoed.to_string().contains("terminate"), "{echoed}");
}

/// 다른 기기 칸에 쓰는 일(raw 입력·칸 세우기)과 대화 기록이 그 기기로 가서 끝난다(`local_only`). 받는 기기의 답이
/// 다른 일·다른 기기 것이면 믿지 않는다.
#[tokio::test]
async fn remote_acts_and_transcript_reach_their_source() {
    use axum::extract::{Path, Query};
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Value)>::new()));
    let (reads, writes) = (seen.clone(), seen.clone());
    let app = axum::Router::new()
        .route("/collab/{op}", axum::routing::get(
            move |Path(op): Path<String>, Query(q): Query<std::collections::HashMap<String, String>>| {
                let reads = reads.clone();
                async move {
                    let params: Value = serde_json::from_str(q.get("params").map(String::as_str).unwrap_or("{}")).unwrap();
                    reads.lock().unwrap().push((op.clone(), params.clone()));
                    axum::Json(match op.as_str() {
                        "board" => json!({"schema_version":1,"scope":"local","panes":[],
                            "sources":[{"machine_id":"fixture-remote-machine","state":"online"}]}),
                        "transcript" => json!({"address":params["address"],"turns":[{"role":"user","text":"hi"}]}),
                        _ => json!({}),
                    })
                }
            }))
        .route("/collab/act", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let writes = writes.clone();
            async move {
                writes.lock().unwrap().push(("act".into(), body.clone()));
                axum::Json(json!({"ok":true,"op":body["op"],"machine_id":"fixture-remote-machine",
                    "address":{"machine_id":"fixture-remote-machine","surface_key":"new","surface_id":"%9"}}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let roster = json!([{"label":"fixture-native","machine_id":"fixture-remote-machine","base":base}]);
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "collab_contract_tests::remote_act_child", "--ignored", "--nocapture"])
            .env("KASATERM_MACHINE_ID", "fixture-local-machine")
            .env("KASATERM_MACHINES", roster.to_string())
            .env("KASATERM_TEST_ACT", "1")
            .output().unwrap()
    }).await.unwrap();
    assert!(output.status.success(), "remote act child failed: {} {}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let seen = seen.lock().unwrap();
    let acts: Vec<&Value> = seen.iter().filter(|(op, _)| op == "act").map(|(_, body)| body).collect();
    assert_eq!(acts.iter().map(|b| b["op"].as_str().unwrap()).collect::<Vec<_>>(), ["send_text", "spawn"]);
    assert!(acts.iter().all(|b| b["local_only"] == true), "every write must terminate at its source");
    let talk = seen.iter().find(|(op, _)| op == "transcript").expect("transcript asked the source");
    assert_eq!(talk.1["local_only"], true);
    server.abort();
}

#[test]
#[ignore = "isolated subprocess invoked by remote act contract test"]
fn remote_act_child() {
    if std::env::var("KASATERM_TEST_ACT").is_err() { return; }
    crate::install_collab_env();
    let backend = crate::standalone::StandaloneBackend::new(std::env::temp_dir());
    let act = |params: Value| kasa_collab::act_service::act(&backend, &params);
    let typed = act(json!({"op":"send_text","address":view_address(),"text":"ls\n"})).unwrap();
    assert_eq!(typed["machine_id"], "fixture-remote-machine");
    let placed = act(json!({"op":"spawn","machine_id":"fixture-remote-machine","window":"new"})).unwrap();
    assert_eq!(placed["address"]["surface_id"], "%9");
    let talk = kasa_collab::board_service::view(&backend, "transcript", &json!({"address":view_address(),"turns":3})).unwrap();
    assert_eq!(talk["turns"][0]["text"], "hi");
    let echoed = act(json!({"op":"focus","address":view_address(),"local_only":true})).unwrap_err();
    assert!(echoed.to_string().contains("terminate"), "{echoed}");
}
