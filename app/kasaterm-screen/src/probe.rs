//! 리그용 보는 쪽 탐침 — 화면 서비스 노드에 `kasa/screen/1` 로 붙어 N 초 받고 장·소리 수와 지연을 한 줄 JSON 으로 낸다
//! (`docs/screen-service-split.md` 「검증 리그」). 사람 앱의 명부·키를 안 쓴다: 탐침 키는 리그 폴더에 두고 그 id 를 서비스 허용
//! 목록에 미리 넣는다.
//!
//! `kasaterm-screen --probe-id <키 파일> <출력 파일>` · `kasaterm-screen --probe-watch <키 파일> <노드 JSON 파일> <초> <출력 파일> [Esc 를 넣을 초]`
//! 결과는 출력 파일에 쓴다 — 릴리스 빌드는 창 서브시스템이라 표준 출력이 없다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use kasa_cast::{Config, Event, SCREEN_ALPN};
use kasa_net::iroh::endpoint::{presets, PortmapperConfig};
use kasa_net::iroh::{Endpoint, RelayMode};
use serde_json::{json, Value};

#[derive(Default)]
struct Tally {
    frames: u64,
    keys: u64,
    audio: u64,
    lat: Vec<f64>,
    net: Vec<f64>,
    info: Value,
    denied: Option<String>,
    closed: Option<String>,
    input: Vec<String>,
    path: String,
}

fn say(path: Option<&String>, text: &str) -> i32 {
    println!("{text}");
    match path {
        Some(p) => i32::from(std::fs::write(p, text).is_err()),
        None => 0,
    }
}

pub fn run(args: &[String]) -> i32 {
    let key = match args.get(1).map(std::path::Path::new).map(kasa_net::identity::load_or_create) {
        Some(Ok(k)) => k,
        _ => {
            eprintln!("탐침 키를 못 읽는다");
            return 2;
        }
    };
    if args.first().map(String::as_str) == Some("--probe-id") {
        return say(args.get(2), &key.public().to_string());
    }
    let node: Value = args.get(2).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let node = node.get("node").cloned().unwrap_or(node);
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);
    let Some(addr) = kasa_net::peer::addr_from_json(&node) else {
        eprintln!("노드 주소를 못 읽는다: {node}");
        return 2;
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(_) => return 2,
    };
    rt.block_on(async move {
        let bind = std::env::var("KASATERM_KASANET_BIND").ok().and_then(|v| v.parse::<std::net::SocketAddr>().ok());
        let mut b = Endpoint::builder(presets::Minimal).secret_key(key).relay_mode(RelayMode::Disabled).portmapper_config(PortmapperConfig::Disabled);
        if let Some(at) = bind {
            b = match b.clear_ip_transports().bind_addr(at) {
                Ok(b) => b,
                Err(_) => return 2,
            };
        }
        let Ok(endpoint) = b.bind().await else { return 2 };
        let tally = Arc::new(Mutex::new(Tally::default()));
        let t = tally.clone();
        // 엔드포인트는 끝까지 쥔다 — 연결 퓨처 안에서 버리면 소켓이 닫혀 스트림이 안 열린다.
        let ep = endpoint.clone();
        let connect = async move {
            let conn = ep.connect(addr, SCREEN_ALPN).await.map_err(|e| format!("연결: {e}"))?;
            Ok((conn, "probe"))
        };
        let watch = kasa_cast::spawn_watch(&tokio::runtime::Handle::current(), connect, Config { audio: true, ..Config::default() }, move |e| {
            let mut t = t.lock().unwrap_or_else(|e| e.into_inner());
            match e {
                Event::Connected { path, .. } => t.path = path.to_string(),
                Event::Info(v) => t.info = v,
                Event::Frame { frame, t_recv, offset } => {
                    t.frames += 1;
                    t.keys += u64::from(frame.key);
                    t.lat.push((t_recv + offset - frame.t_cap) * 1000.0);
                    t.net.push((t_recv + offset - frame.t_enc) * 1000.0);
                }
                Event::Audio { .. } => t.audio += 1,
                Event::Denied(why) => t.denied = Some(why),
                Event::Closed(why) => t.closed = Some(why),
                Event::Input { ok, reason } => t.input.push(if ok { "ok".into() } else { reason.unwrap_or_default() }),
            }
        });
        // 보안 데스크톱(UAC) 따라가기 시험 — 그 초에 Esc 를 넣어 떠 있는 확인 창을 닫는다.
        let escape: Option<u64> = args.get(5).and_then(|s| s.parse().ok()).filter(|e| *e < secs);
        let mut per_sec = Vec::new();
        for i in 0..secs {
            if escape == Some(i) {
                watch.send_input(&json!({ "t": "key", "k": "Escape" }));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            per_sec.push(tally.lock().unwrap_or_else(|e| e.into_inner()).frames);
        }
        drop(watch);
        endpoint.close().await;
        let mut t = tally.lock().unwrap_or_else(|e| e.into_inner());
        let pct = |v: &mut Vec<f64>, q: f64| {
            v.sort_by(f64::total_cmp);
            v.get(((v.len() as f64 - 1.0) * q).round().max(0.0) as usize).copied().unwrap_or(f64::NAN)
        };
        let out = json!({
            "frames": t.frames, "keys": t.keys, "audio_chunks": t.audio, "fps": t.frames as f64 / secs as f64,
            "cap_to_recv_ms": [pct(&mut t.lat, 0.5), pct(&mut t.lat, 0.9)],
            "enc_to_recv_ms": [pct(&mut t.net, 0.5), pct(&mut t.net, 0.9)],
            "denied": t.denied, "closed": t.closed, "path": t.path, "info": t.info, "frames_by_second": per_sec, "input": t.input,
        });
        let code = say(args.get(4), &out.to_string());
        if t.frames == 0 {
            1
        } else {
            code
        }
    })
}
