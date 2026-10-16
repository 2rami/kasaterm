//! 소켓 끝에서 끝까지 — 진짜 셸을 띄우고 부르는 쪽이 보내는 그대로 요청한다.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use kasa_host::{server, Host};
use serde_json::{json, Value};

static N: AtomicUsize = AtomicUsize::new(0);

fn start() -> PathBuf {
    let path = std::env::temp_dir().join(format!("kh-{}-{}.sock", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let listener = server::bind(&path).unwrap();
    std::thread::spawn(move || server::serve(listener, Host::new()));
    path
}

struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    fn new(path: &PathBuf) -> Self {
        let writer = UnixStream::connect(path).unwrap();
        Self { reader: BufReader::new(writer.try_clone().unwrap()), writer }
    }

    fn raw(&mut self, method: &str, params: Value) -> Value {
        writeln!(self.writer, "{}", json!({"id": 7, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], 7);
        reply
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let reply = self.raw(method, params);
        assert_eq!(reply["ok"], true, "{method}: {reply}");
        reply["result"].clone()
    }

    fn spawn_sh(&mut self, extra: Value) -> String {
        let mut params = json!({"shell": "/bin/sh", "cols": 80, "rows": 24, "name": "나쵸 · 시험"});
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        self.call("spawn", params)["id"].as_str().unwrap().to_string()
    }

    fn wait_text(&mut self, id: &str, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = self.call("text", json!({"id": id}))["text"].as_str().unwrap().to_string();
            if text.contains(needle) {
                return text;
            }
            assert!(Instant::now() < deadline, "{needle:?} 가 안 나왔다:\n{text}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn wait_gone(&mut self, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.call("list", json!({})).as_array().unwrap().iter().any(|s| s["id"] == id) {
            assert!(Instant::now() < deadline, "{id} 가 목록에 남았다");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[test]
fn 띄운_셸에_명령이_쳐져_돈다() {
    let mut c = Client::new(&start());
    assert!(c.call("ping", json!({}))["version"].is_string());
    let id = c.spawn_sh(json!({"cmd": "echo kasa-host-$((40+2))"}));
    c.wait_text(&id, "kasa-host-42");
    let listed = c.call("list", json!({}));
    assert_eq!(listed[0]["id"], id.as_str());
    assert_eq!(listed[0]["name"], "나쵸 · 시험");
}

#[test]
fn 세션은_자기_번호를_env_로_안다() {
    let mut c = Client::new(&start());
    let id = c.spawn_sh(json!({"env": {"NACHO_SEAT": "자리-1"}, "cmd": "echo \"$KASA_HOST_SESSION/$NACHO_SEAT\""}));
    c.wait_text(&id, &format!("{id}/자리-1"));
}

#[test]
fn 화면은_grid_프레임이고_바뀌면_롱폴이_깬다() {
    let path = start();
    let mut c = Client::new(&path);
    let id = c.spawn_sh(json!({"cmd": "echo ready"}));
    c.wait_text(&id, "ready");
    let first = c.call("screen", json!({"id": id}));
    assert_eq!(first["frame"]["t"], "grid");
    assert_eq!(first["frame"]["cols"], 80);
    assert_eq!(first["frame"]["rows"], 24);
    assert_eq!(first["alive"], true);
    let hash = first["hash"].as_str().unwrap().to_string();

    let (poke_path, poke_id) = (path.clone(), id.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        Client::new(&poke_path).call("write", json!({"id": poke_id, "data": "echo poke\r"}));
    });
    let started = Instant::now();
    let next = c.call("screen", json!({"id": id, "since": hash, "wait": 10}));
    assert_ne!(next["hash"], hash.as_str());
    assert!(started.elapsed() < Duration::from_secs(9), "바뀌었는데 기다림을 다 채웠다");
}

#[test]
fn 붙여넣기는_제출까지_키는_이름으로() {
    let mut c = Client::new(&start());
    let id = c.spawn_sh(json!({}));
    c.call("paste", json!({"id": id, "text": "echo pasted-$((1+1))", "submit": true}));
    c.wait_text(&id, "pasted-2");
    c.call("write", json!({"id": id, "data": "echo keyed"}));
    c.call("key", json!({"id": id, "name": "enter"}));
    c.wait_text(&id, "\nkeyed");
    let bad = c.raw("key", json!({"id": id, "name": "hyper"}));
    assert_eq!(bad["ok"], false);
}

#[test]
fn 크기_이름_닫기() {
    let mut c = Client::new(&start());
    let id = c.spawn_sh(json!({}));
    c.call("resize", json!({"id": id, "cols": 100, "rows": 30}));
    c.call("rename", json!({"id": id, "name": "다른 이름"}));
    let row = c.call("list", json!({}))[0].clone();
    assert_eq!((row["cols"].as_u64(), row["rows"].as_u64()), (Some(100), Some(30)));
    assert_eq!(row["name"], "다른 이름");
    c.call("close", json!({"id": id}));
    assert!(c.call("list", json!({})).as_array().unwrap().is_empty());
    assert_eq!(c.raw("text", json!({"id": id}))["ok"], false);
}

#[test]
fn 셸이_끝나면_목록에서_빠진다() {
    let mut c = Client::new(&start());
    let id = c.spawn_sh(json!({}));
    c.call("write", json!({"id": id, "data": "exit\r"}));
    c.wait_gone(&id);
}

#[test]
fn 살아_있는_호스트_자리는_못_뺏는다() {
    let path = start();
    assert!(server::bind(&path).is_err());
    let mut c = Client::new(&path);
    assert_eq!(c.raw("nope", json!({}))["ok"], false);
}

#[test]
fn 화면_뒤_입력이_바뀌면_자동_붙여넣기를_거절한다() {
    let mut c = Client::new(&start());
    let id = c.spawn_sh(json!({}));
    let frame = c.call("screen", json!({"id": id}));
    c.call("write", json!({"id": id, "data": "draft"}));
    std::thread::sleep(Duration::from_millis(350));
    let reply = c.raw("paste", json!({"id": id, "text": "automatic", "submit": true,
        "expected_revision": frame["input_revision"]}));
    assert_eq!(reply["ok"], false);
    let text = c.call("text", json!({"id": id}));
    assert!(!text["text"].as_str().unwrap().contains("automatic"));
    c.call("close", json!({"id": id}));
}

#[test]
fn 붙여넣기_중_사람_초안은_자동으로_제출되지_않는다() {
    let path = start();
    let mut c = Client::new(&path);
    let id = c.spawn_sh(json!({"cmd": "stty -echo; PS1=''; echo READY"}));
    c.wait_text(&id, "\nREADY");
    let (other_path, other_id) = (path.clone(), id.clone());
    let sender = std::thread::spawn(move || {
        Client::new(&other_path).call("paste", json!({"id": other_id, "text": "echo AUTO", "submit": true}));
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let f = c.call("screen", json!({"id": id}));
        if f["input_revision"].as_u64().unwrap() >= 2 { break; }
        assert!(Instant::now() < deadline);
    }
    c.call("write", json!({"id": id, "data": "echo HUMAN"}));
    sender.join().unwrap();
    let text = c.wait_text(&id, "\nAUTO");
    assert!(!text.contains("\nHUMAN"));
    c.call("key", json!({"id": id, "name": "enter"}));
    c.wait_text(&id, "\nHUMAN");
    c.call("close", json!({"id": id}));
}
