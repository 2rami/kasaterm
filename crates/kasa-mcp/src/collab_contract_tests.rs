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
