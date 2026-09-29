//! C ABI 를 폰 앱이 부르는 순서 그대로 부른다 — 루프백 데스크톱 하나에 입구를 열고 직통으로 싣고, 직통을 잃으면 닫는다.

use std::ffi::{c_char, CStr, CString};
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use kasa_net::iroh::endpoint::{presets, PortmapperConfig};
use kasa_net::iroh::protocol::Router;
use kasa_net::iroh::{EndpointId, RelayMode, SecretKey};
use kasa_net::{fwd, AllowList, FwdServer};
use kasa_net_ffi::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const T: Duration = Duration::from_secs(20);

fn take(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { kasanet_free_string(p) };
    Some(s)
}

fn path_of(local: u16) -> String {
    let v: serde_json::Value = serde_json::from_str(&take(kasanet_state(local)).unwrap()).unwrap();
    v["path"].as_str().unwrap().to_string()
}

async fn wait_path(local: u16, want: bool) {
    let start = Instant::now();
    while (path_of(local) == "direct") != want {
        assert!(start.elapsed() < T, "길이 시간 안에 안 바뀌었다");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn echo() -> u16 {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

async fn round_trip(port: u16) -> std::io::Result<Vec<u8>> {
    let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?;
    s.write_all(b"kasa").await?;
    let mut buf = [0u8; 4];
    tokio::time::timeout(T, s.read_exact(&mut buf)).await??;
    Ok(buf.to_vec())
}

#[tokio::test(flavor = "multi_thread")]
async fn phone_abi_carries_only_while_direct() {
    let dir = tempfile_dir();
    let key = CString::new(dir.join("kasanet.key").to_str().unwrap()).unwrap();
    // 앱은 런타임 밖 스레드에서 부른다 — 시험도 그렇게.
    let started = tokio::task::spawn_blocking(move || unsafe {
        [kasanet_start(key.as_ptr()), kasanet_start(key.as_ptr())]
    })
    .await
    .unwrap();
    assert_eq!(started, [0, 0], "두 번 불러도 된다");
    let phone: EndpointId = take(kasanet_id()).unwrap().parse().unwrap();

    let echo_port = echo().await;
    let allow = AllowList::default();
    let server_ep = kasa_net::builder(presets::Minimal, SecretKey::generate(), &allow)
        .relay_mode(RelayMode::Disabled)
        .portmapper_config(PortmapperConfig::Disabled)
        .clear_ip_transports()
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let at = server_ep
        .bound_sockets()
        .into_iter()
        .find(SocketAddr::is_ipv4)
        .unwrap();
    let peer = serde_json::json!({"id": server_ep.id().to_string(), "addrs": [at.to_string()], "port": echo_port});
    let server = Router::builder(server_ep)
        .accept(fwd::ALPN, FwdServer::new([echo_port]))
        .spawn();
    allow.insert(phone);

    let peer = CString::new(peer.to_string()).unwrap();
    let local = unsafe { kasanet_open(peer.as_ptr()) };
    assert!(local > 0, "{:?}", take(kasanet_last_error()));
    assert_eq!(
        unsafe { kasanet_open(peer.as_ptr()) },
        local,
        "같은 데스크톱이면 같은 입구"
    );
    let local = local as u16;

    wait_path(local, true).await;
    assert_eq!(round_trip(local).await.unwrap(), b"kasa");

    server.shutdown().await.unwrap();
    wait_path(local, false).await;
    assert!(
        round_trip(local).await.is_err(),
        "직통이 아니면 입구는 싣지 않는다"
    );

    kasanet_close(local);
    assert!(take(kasanet_state(local)).is_none());
    let bad = CString::new("{\"id\":\"nope\"}").unwrap();
    assert_eq!(unsafe { kasanet_open(bad.as_ptr()) }, -1);
    assert!(take(kasanet_last_error()).is_some());
    tokio::task::spawn_blocking(|| kasanet_stop())
        .await
        .unwrap();
    assert!(take(kasanet_id()).is_none());
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("kasanet-ffi-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
